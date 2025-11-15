use anyhow::Context;
use std::process::{Command, ExitStatus};

#[derive(Debug)]
pub struct SayCommand {
    args: Vec<String>,
}

impl SayCommand {
    #[must_use]
    pub fn new(args: Vec<String>) -> Self {
        Self { args }
    }

    pub fn run(&self, message: &str) -> anyhow::Result<ExitStatus> {
        let mut cmd = Command::new("say");
        cmd.args(&self.args);
        cmd.arg(message)
            .status()
            .context("Command::status() failed")
    }
}
