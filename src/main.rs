use crate::notification::{deliver, set_global_delegate};
use anyhow::Context;
use nix::pty::{ForkptyResult, forkpty};
use nix::sys::signal::{SigHandler, SigSet, Signal, signal};
use nix::sys::termios::{SetArg, Termios, cfmakeraw, tcgetattr, tcsetattr};
use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};
use nix::unistd::{Pid, execvp};
use nix::{ioctl_read_bad, ioctl_write_ptr_bad};
use objc2_foundation::{NSDate, NSDefaultRunLoopMode, NSRunLoop};
use std::borrow::Cow;
use std::ffi::CString;
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::os::unix::ffi::OsStringExt;
use std::process::{Command, ExitStatus};
use std::sync::OnceLock;
use std::thread;

mod notification;

const BUNDLE_IDENTIFIER: &str = "com.apple.Terminal";
const CONFIRMATIONS: &str = concat!(
    r"(?:\A|[\r\n])(?:\x1b\[[0-?][ -?]*[@-~])*│(?:\x1b\[[0-?][ -?]*[@-~]|\s)*",
    r"((?:Do you want|Would you like) to [^?\n]+\?)",
);
const PROMPTS: &str = r"(?:\A|[\r\n])(?:\x1b\[[0-?][ -?]*[@-~])*│(?:\x1b\[[0-?][ -?]*[@-~])*\s>\s";
const NOTIFICATION_TITLE: &str = "Claude Code";
const VOICE: &str = "Samantha";

fn main() -> anyhow::Result<()> {
    match unsafe { forkpty(None, None) }.context("forkpty() failed")? {
        ForkptyResult::Child => {
            let mut argv = vec![c"claude".to_owned()];
            for arg in std::env::args_os().skip(1) {
                argv.push(CString::new(arg.into_vec())?);
            }
            execvp(&argv[0], &argv).context("execvp() failed")?;
            unreachable!();
        }
        ForkptyResult::Parent { child, master } => intercept(child, master),
    }
}

fn intercept(child: Pid, master: OwnedFd) -> anyhow::Result<()> {
    let _termios = try_make_raw(io::stdin()).context("try_make_raw")?;
    let confirmations = regex::bytes::Regex::new(CONFIRMATIONS)?;
    let prompts = regex::bytes::Regex::new(PROMPTS)?;
    let mut reader = File::from(master.try_clone()?);
    let mut writer = File::from(master.try_clone()?);

    set_global_delegate().context("set_global_delegate")?;
    spawn_winsize_updater(master).context("spawn_winsize_updater")?;
    thread::spawn(move || io::copy(&mut io::stdin(), &mut writer));

    let (notification_tx, notification_rx) = std::sync::mpsc::sync_channel::<String>(10);

    thread::spawn(move || {
        let mut stdout = io::stdout().lock();
        let mut buffer = [0u8; 4096];
        let mut window: Vec<u8> = Vec::with_capacity(2 * buffer.len());
        let mut notified = false;

        while let Ok(n) = reader.read(&mut buffer) {
            if n == 0 {
                break;
            }
            if stdout.write_all(&buffer[..n]).is_err() {
                break;
            }
            if stdout.flush().is_err() {
                break;
            }

            window.extend_from_slice(&buffer[..n]);
            if let Some(matched) = confirmations.captures(&window).and_then(|c| c.get(1)) {
                let confirmation = matched.as_bytes();
                if !notified {
                    if let Ok(confirmation_str) = std::str::from_utf8(confirmation) {
                        let _ = notification_tx.try_send(confirmation_str.to_string());
                    }
                    notified = true;
                }
                window.clear();
            } else if prompts.find(&window).is_some() {
                notified = false;
                window.clear();
            } else if window.len() > window.capacity() / 2 {
                window.drain(0..(window.len() / 2));
            }
        }
    });

    thread::spawn(move || {
        while let Ok(message) = notification_rx.recv() {
            let message = strip_ascii_escape_sequences(&message);
            let _ = deliver(NOTIFICATION_TITLE, &message);
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

fn strip_ascii_escape_sequences(text: &str) -> Cow<str> {
    static ASCII_ESCAPE_SEQUENCES: OnceLock<regex::Regex> = OnceLock::new();
    ASCII_ESCAPE_SEQUENCES
        .get_or_init(|| {
            regex::Regex::new(r"\x1b\[[0-?][ -?]*[@-~]")
                .context("failed to initialize ASCII_ESCAPE_SEQUENCES")
                .unwrap()
        })
        .replace_all(text, "")
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

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bash_command_v_1_0_35() {
        assert_eq!(
            capture(b"\n\x1b[38;5;153m\xe2\x94\x82\x1b[39m Do you want to proceed?"),
            Some(b"Do you want to proceed?".as_slice()),
        )
    }

    #[test]
    fn ready_to_code_v_1_0_35() {
        assert_eq!(
            capture(
                b"\n\x1b[38;5;73m\xe2\x94\x82\x1b[39m \x1b[38;5;246mWould you like to proceed?",
            ),
            Some(b"Would you like to proceed?".as_slice()),
        )
    }

    fn capture(input: &[u8]) -> Option<&[u8]> {
        regex::bytes::Regex::new(CONFIRMATIONS)
            .unwrap()
            .captures(input)
            .and_then(|c| c.get(1))
            .map(|m| m.as_bytes())
    }
}
