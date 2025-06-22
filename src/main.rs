use anyhow::{Context, ensure};
use mac_notification_sys::{send_notification, set_application};
use nix::pty::{ForkptyResult, forkpty};
use nix::sys::signal::{SigHandler, SigSet, Signal, signal};
use nix::sys::termios::{SetArg, Termios, cfmakeraw, tcgetattr, tcsetattr};
use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};
use nix::unistd::{Pid, execvp};
use nix::{ioctl_read_bad, ioctl_write_ptr_bad};
use std::borrow::Cow;
use std::ffi::CString;
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::os::unix::ffi::OsStringExt;
use std::process::Command;
use std::sync::OnceLock;
use std::thread;

const BUNDLE_IDENTIFIER: &str = "com.apple.Terminal";
const CONFIRMATIONS: &str = concat!(
    r"(?:\A|[\r\n])(?:\x1b\[[0-?][ -?]*[@-~])*│(?:\x1b\[[0-?][ -?]*[@-~])*\s",
    r"(Do you want to (?:create|make this edit to|proceed)(?:\s+[^?\n]+)?\?)",
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
    let mut stdout = io::stdout().lock();
    let mut window: Vec<u8> = Vec::with_capacity(8192);
    let mut buffer = [0u8; 4096];
    let mut notified = false;

    set_application(BUNDLE_IDENTIFIER).context("mac_notification_sys::set_application() failed")?;
    spawn_winsize_updater(master).context("spawn_winsize_updater")?;
    thread::spawn(move || io::copy(&mut io::stdin(), &mut writer));

    loop {
        let n = reader.read(&mut buffer).context("read() failed")?;
        if n == 0 {
            break;
        }
        stdout
            .write_all(&buffer[..n])
            .context("write_all() failed")?;
        stdout.flush().context("flush() failed")?;

        window.extend_from_slice(&buffer[..n]);
        if let Some(matched) = confirmations.captures(&window).and_then(|c| c.get(1)) {
            let confirmation = matched.as_bytes();
            if !notified {
                if let Ok(confirmation) =
                    std::str::from_utf8(confirmation).map(strip_ascii_escape_sequences)
                {
                    let _ = send_notification(NOTIFICATION_TITLE, None, &confirmation, None);
                    let _ = say(&confirmation);
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

    let status = waitpid(child, Some(WaitPidFlag::empty()))?;
    ensure!(
        matches!(status, WaitStatus::Exited(_, 0)),
        "claude did not exit normally: {status:?}",
    );
    Ok(())
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

fn say(message: &str) -> anyhow::Result<()> {
    Command::new("say")
        .arg("-v")
        .arg(VOICE)
        .arg(message)
        .spawn()
        .context("Command::spawn() failed")?;
    Ok(())
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
