use crate::runtime::Runtime;
use crate::tty_text::buffer::Buffer;
use crate::tty_text::fragment::EscapeSequence;
use anyhow::Context;
use macos::notification::{deliver_if_osc9_unsupported, set_global_delegate};
use nix::pty::{ForkptyResult, forkpty};
use nix::sys::signal::{SigHandler, SigSet, Signal, signal};
use nix::sys::termios::{SetArg, Termios, cfmakeraw, tcgetattr, tcsetattr};
use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};
use nix::unistd::Pid;
use nix::{ioctl_read_bad, ioctl_write_ptr_bad};
use objc2_foundation::{NSDate, NSDefaultRunLoopMode, NSRunLoop};
use std::convert::Infallible;
use std::fs::File;
use std::io::{self, Write};
use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::sync::atomic::AtomicU16;
use std::thread;

mod args;
mod claude;
mod macos;
mod runtime;
mod tty_text;

const DEFAULT_NOTIFICATION_TITLE: &str = "Claude Code";

static TERMINAL_WIDTH: AtomicU16 = AtomicU16::new(0);

fn main() -> anyhow::Result<Infallible> {
    let runtime = args::Arguments::parse()?.try_into_runtime()?;

    if runtime.claude_command.should_bypass_pty() {
        runtime.claude_command.exec()?;
    }

    match unsafe { forkpty(None, None) }.context("forkpty() failed")? {
        ForkptyResult::Child => runtime.claude_command.exec(),
        ForkptyResult::Parent { child, master } => {
            std::process::exit(intercept(child, master, runtime)?)
        }
    }
}

fn intercept(child: Pid, master: OwnedFd, mut runtime: Runtime) -> anyhow::Result<i32> {
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
        let mut title = DEFAULT_NOTIFICATION_TITLE.to_string();
        let mut buffer = Buffer::<8192>::new();

        while let Ok(n) = buffer.extend_from_read(&mut reader) {
            if n == 0 {
                break;
            }

            runtime
                .reformatter
                .set_terminal_width(TERMINAL_WIDTH.load(std::sync::atomic::Ordering::Relaxed));
            for fragment in buffer.read_fragments(&runtime.reformatter) {
                if stdout.write_all(fragment.data()).is_err() {
                    return;
                }
                match fragment.escape_sequence() {
                    Some(EscapeSequence::SetWindowAndIconTitle(new_title)) => {
                        title.replace_range(.., &String::from_utf8_lossy(new_title.trim_ascii()));
                    }
                    Some(EscapeSequence::PostNotification(message)) => {
                        let message = String::from_utf8_lossy(message.trim_ascii()).into_owned();
                        let _ = notification_tx.try_send((title.clone(), message));
                    }
                    Some(
                        EscapeSequence::EndSynchronizedUpdate
                        | EscapeSequence::ShowCursor
                        | EscapeSequence::Incomplete
                        | EscapeSequence::Other,
                    )
                    | None => {}
                }
            }

            if stdout.flush().is_err() {
                break;
            }
        }
    });

    let notification_center_delivery_enabled = runtime.notification_center_delivery_enabled;
    let say_command = runtime.say_command;
    thread::spawn(move || {
        while let Ok((title, message)) = notification_rx.recv() {
            if notification_center_delivery_enabled {
                let _ = deliver_if_osc9_unsupported(&title, &message);
            }
            if let Some(say_command) = &say_command {
                let _ = say_command.run(&message);
            }
        }
    });

    unsafe {
        let run_loop = NSRunLoop::mainRunLoop();
        loop {
            match waitpid(child, Some(WaitPidFlag::WNOHANG)) {
                Ok(WaitStatus::Exited(_, code)) => return Ok(code),
                Ok(WaitStatus::Signaled(_, signal, _)) => return Ok(128 + signal as i32),
                Ok(WaitStatus::StillAlive) => {}
                Ok(status) => anyhow::bail!("unexpected status: {status:?}"),
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
