use anyhow::Context;
use nix::unistd::execvp;
use std::convert::Infallible;
use std::ffi::{CString, NulError, OsString};
use std::os::unix::ffi::OsStringExt;

#[derive(Debug)]
pub struct ClaudeCommand {
    argv: Vec<CString>,
}

impl TryFrom<Vec<OsString>> for ClaudeCommand {
    type Error = anyhow::Error;

    fn try_from(argv: Vec<OsString>) -> Result<Self, Self::Error> {
        anyhow::ensure!(!argv.is_empty(), "argv cannot be empty");

        let argv_cstring = argv
            .into_iter()
            .map(|arg| CString::new(arg.into_vec()))
            .collect::<Result<Vec<_>, NulError>>()
            .context("argument contains null byte")?;

        Ok(Self { argv: argv_cstring })
    }
}

impl ClaudeCommand {
    pub fn should_bypass_pty(&self) -> bool {
        self.argv.iter().skip(1).any(|arg| {
            matches!(
                arg.to_str().ok(),
                Some("-p" | "--print" | "-v" | "--version" | "-h" | "--help"),
            )
        })
    }

    pub fn exec(&self) -> anyhow::Result<Infallible> {
        execvp(&self.argv[0], &self.argv).context("execvp() failed")
    }
}
