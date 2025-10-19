use crate::notification::{deliver_if_osc777_unsupported, set_global_delegate};
use crate::tty_text::buffer::Buffer;
use crate::tty_text::fragment::EscapeSequence;
use anyhow::Context;
use nix::pty::{ForkptyResult, forkpty};
use nix::sys::signal::{SigHandler, SigSet, Signal, signal};
use nix::sys::termios::{SetArg, Termios, cfmakeraw, tcgetattr, tcsetattr};
use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};
use nix::unistd::{Pid, execvp};
use nix::{ioctl_read_bad, ioctl_write_ptr_bad};
use objc2_foundation::{NSDate, NSDefaultRunLoopMode, NSRunLoop};
use std::ffi::CString;
use std::fs::File;
use std::io::{self, Write};
use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::os::unix::ffi::OsStringExt;
use std::process::{Command, ExitStatus};
use std::sync::atomic::AtomicU16;
use std::thread;

mod notification;
mod tty_text;

const VOICE: &str = "Samantha";

static TERMINAL_WIDTH: AtomicU16 = AtomicU16::new(0);

fn main() -> anyhow::Result<()> {
    if should_bypass_pty(std::env::args_os().skip(1)) {
        let argv = build_argv();
        execvp(&argv[0], &argv).context("execvp() failed")?;
        unreachable!();
    }

    match unsafe { forkpty(None, None) }.context("forkpty() failed")? {
        ForkptyResult::Child => {
            let argv = build_argv();
            execvp(&argv[0], &argv).context("execvp() failed")?;
            unreachable!();
        }
        ForkptyResult::Parent { child, master } => intercept(child, master),
    }
}

fn should_bypass_pty(mut args: impl Iterator<Item = std::ffi::OsString>) -> bool {
    args.any(|arg| {
        matches!(
            arg.to_str(),
            Some("-p" | "--print" | "-v" | "--version" | "-h" | "--help"),
        )
    })
}

fn build_argv() -> Vec<CString> {
    let mut argv = vec![c"claude".to_owned()];
    for arg in std::env::args_os().skip(1) {
        argv.push(CString::new(arg.into_vec()).unwrap());
    }
    argv
}

fn intercept(child: Pid, master: OwnedFd) -> anyhow::Result<()> {
    let _termios = try_make_raw(io::stdin()).context("try_make_raw")?;
    let mut reader = File::from(master.try_clone()?);
    let mut writer = File::from(master.try_clone()?);

    set_global_delegate().context("set_global_delegate")?;
    spawn_winsize_updater(master).context("spawn_winsize_updater")?;
    thread::spawn(move || io::copy(&mut io::stdin(), &mut writer));

    debug_assert!(TERMINAL_WIDTH.load(std::sync::atomic::Ordering::Relaxed) > 0);

    let (notification_tx, notification_rx) = std::sync::mpsc::sync_channel::<(String, String)>(10);

    thread::spawn(move || {
        let mut stdout = io::stdout().lock();
        let mut buffer = Buffer::<8192>::new();

        while let Ok(n) = buffer.extend_from_read(&mut reader) {
            if n == 0 {
                break;
            }

            for fragment in buffer.parse(TERMINAL_WIDTH.load(std::sync::atomic::Ordering::Relaxed))
            {
                if stdout.write_all(fragment.data()).is_err() {
                    return;
                }
                if let Some(EscapeSequence::ShowDesktopNotification(title, body)) =
                    fragment.escape_sequence()
                {
                    let title = String::from_utf8_lossy(title.trim_ascii());
                    let body = String::from_utf8_lossy(body.trim_ascii());
                    let _ = notification_tx.try_send((title.into_owned(), body.into_owned()));
                }
            }

            if stdout.flush().is_err() {
                break;
            }
        }
    });

    thread::spawn(move || {
        while let Ok((title, message)) = notification_rx.recv() {
            let _ = deliver_if_osc777_unsupported(&title, &message);
            let _ = say(&message);
        }
    });

    let (exit_tx, exit_rx) = std::sync::mpsc::channel();

    thread::spawn(move || {
        let status = waitpid(child, Some(WaitPidFlag::empty()));
        let _ = exit_tx.send(status);
    });

    unsafe {
        let run_loop = NSRunLoop::mainRunLoop();
        loop {
            match exit_rx.try_recv() {
                Ok(Ok(WaitStatus::Exited(_, 0))) => return Ok(()),
                Ok(Ok(status)) => anyhow::bail!("claude did not exit normally: {:?}", status),
                Ok(Err(e)) => anyhow::bail!(e),
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Err(e) => anyhow::bail!(e),
            }
            run_loop.runMode_beforeDate(
                NSDefaultRunLoopMode,
                &NSDate::dateWithTimeIntervalSinceNow(0.1),
            );
        }
    }
}

struct TermiosGuard<Fd: AsFd>(Fd, Termios);

impl<Fd: AsFd> Drop for TermiosGuard<Fd> {
    fn drop(&mut self) {
        let _ = tcsetattr(self.0.as_fd(), SetArg::TCSANOW, &self.1);
    }
}

fn try_make_raw<Fd: AsFd>(fd: Fd) -> anyhow::Result<TermiosGuard<Fd>> {
    let termios = tcgetattr(fd.as_fd()).context("tcgetattr() failed")?;
    let mut new_termios = termios.clone();
    cfmakeraw(&mut new_termios);
    tcsetattr(fd.as_fd(), SetArg::TCSANOW, &new_termios).context("tcsetattr() failed")?;
    Ok(TermiosGuard(fd, termios))
}

fn say(message: &str) -> anyhow::Result<ExitStatus> {
    Command::new("say")
        .arg("-v")
        .arg(VOICE)
        .arg(message)
        .status()
        .context("Command::status() failed")
}

fn spawn_winsize_updater<Fd: AsRawFd + Send + Sync + 'static>(fd: Fd) -> anyhow::Result<()> {
    update_winsize(&fd).context("update_winsize() failed")?;

    // On macOS, sigwait() requires signals to be "blocked, but not ignored" (man sigwait).
    // Setting an empty handler ensures the signal is not ignored and makes sigwait() happy.
    extern "C" fn noop(_: nix::libc::c_int) {}
    unsafe { signal(Signal::SIGWINCH, SigHandler::Handler(noop)) }
        .context("failed to set SIGWINCH handler")?;

    let mut sigset = SigSet::empty();
    sigset.add(Signal::SIGWINCH);
    sigset.thread_block().context("failed to block SIGWINCH")?;

    thread::spawn(move || {
        loop {
            match sigset.wait() {
                Ok(Signal::SIGWINCH) => {
                    let _ = update_winsize(&fd);
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
    });

    Ok(())
}

fn update_winsize<Fd: AsRawFd>(fd: &Fd) -> anyhow::Result<()> {
    ioctl_read_bad!(get_winsize, nix::libc::TIOCGWINSZ, nix::libc::winsize);
    ioctl_write_ptr_bad!(set_winsize, nix::libc::TIOCSWINSZ, nix::libc::winsize);

    let stdin = io::stdin().as_fd().as_raw_fd();
    let fd = fd.as_raw_fd();
    let mut winsize = nix::libc::winsize {
        ws_row: 0,
        ws_col: 0,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };

    unsafe { get_winsize(stdin, &mut winsize) }.context("get_winsize() failed")?;
    unsafe { set_winsize(fd, &winsize) }.context("set_winsize() failed")?;

    TERMINAL_WIDTH.store(winsize.ws_col, std::sync::atomic::Ordering::Relaxed);

    Ok(())
}
