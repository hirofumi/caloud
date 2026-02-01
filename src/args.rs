use crate::claude::ClaudeCommand;
use crate::input_rewrite::rewriter::InputRewriter;
use crate::input_rewrite::rule::RewriteRule;
use crate::macos::say::SayCommand;
use crate::runtime::Runtime;
use crate::tty_text::reformat::{LineWrapMode, Reformatter};
use anyhow::{Context, bail};
use lexopt::prelude::*;
use std::ffi::OsString;

#[derive(Debug)]
pub struct Arguments {
    notification_center_delivery_enabled: bool,
    say_args: Option<OsString>,
    line_wrap_mode: LineWrapMode,
    rewrite_rules: Vec<RewriteRule>,
    claude_argv: Vec<OsString>,
}

impl Arguments {
    pub fn parse() -> anyhow::Result<Self> {
        parse_args(std::env::args_os())
    }

    pub fn try_into_runtime(self) -> anyhow::Result<Runtime> {
        Ok(Runtime {
            notification_center_delivery_enabled: self.notification_center_delivery_enabled,
            say_command: self.say_args.map(Self::try_build_say_command).transpose()?,
            input_rewriter: InputRewriter::new(self.rewrite_rules),
            reformatter: Reformatter::new(0, self.line_wrap_mode),
            claude_command: Self::try_build_claude_command(self.claude_argv)?,
        })
    }

    fn try_build_say_command(say_args: OsString) -> anyhow::Result<SayCommand> {
        shell_words::split(
            say_args
                .to_str()
                .context("say argument contains invalid UTF-8")?,
        )
        .map(SayCommand::new)
        .context("failed to parse say command arguments")
    }

    fn try_build_claude_command(mut claude_argv: Vec<OsString>) -> anyhow::Result<ClaudeCommand> {
        if claude_argv.is_empty() {
            claude_argv.push(OsString::from("claude"));
        }
        ClaudeCommand::try_from(claude_argv).context("failed to build argv for claude")
    }
}

fn parse_args(args: impl IntoIterator<Item = impl Into<OsString>>) -> anyhow::Result<Arguments> {
    let mut notification_center_delivery_enabled = false;
    let mut say_args = None;
    let mut line_wrap_mode = LineWrapMode::Preserve;
    let mut rewrite_rules: Vec<RewriteRule> = Vec::new();
    let mut claude_argv: Vec<OsString> = Vec::new();

    let mut parser = lexopt::Parser::from_iter(args);
    while let Some(arg) = parser.next()? {
        match arg {
            Long("notification-center") => {
                notification_center_delivery_enabled = true;
            }
            Long("say") => {
                say_args = Some(parser.value()?);
            }
            Long("line-wrap") => {
                let value = parser.value()?.string()?;
                line_wrap_mode = match value.as_str() {
                    "adjust" => LineWrapMode::Adjust,
                    "preserve" => LineWrapMode::Preserve,
                    _ => bail!("invalid value for --line-wrap: {}", value),
                };
            }
            Long("input-rewrite") => {
                let value = parser.value()?.string()?;
                let rule = RewriteRule::parse(&value)
                    .with_context(|| format!("failed to parse --input-rewrite: {}", value))?;
                rewrite_rules.push(rule);
            }
            Value(val) => {
                claude_argv.push(val);
            }
            _ => return Err(arg.unexpected().into()),
        }
    }

    Ok(Arguments {
        notification_center_delivery_enabled,
        say_args,
        line_wrap_mode,
        rewrite_rules,
        claude_argv,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_values() {
        let arguments = parse_args(["prog"]).unwrap();
        assert!(!arguments.notification_center_delivery_enabled);
        assert!(arguments.say_args.is_none());
        assert_eq!(arguments.line_wrap_mode, LineWrapMode::Preserve);
    }

    #[test]
    fn notification_center_option() {
        let arguments = parse_args(["prog", "--notification-center"]).unwrap();
        assert!(arguments.notification_center_delivery_enabled);
    }

    #[test]
    fn say_option() {
        let arguments = parse_args(["prog", "--say=-v Samantha"]).unwrap();
        assert_eq!(arguments.say_args, Some(OsString::from("-v Samantha")));
    }

    #[test]
    fn line_wrap_adjust() {
        let arguments = parse_args(["prog", "--line-wrap=adjust"]).unwrap();
        assert_eq!(arguments.line_wrap_mode, LineWrapMode::Adjust);
    }

    #[test]
    fn line_wrap_preserve() {
        let arguments = parse_args(["prog", "--line-wrap=preserve"]).unwrap();
        assert_eq!(arguments.line_wrap_mode, LineWrapMode::Preserve);
    }

    #[test]
    fn invalid_line_wrap_option() {
        let result = parse_args(["prog", "--line-wrap=invalid"]);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("invalid value"));
    }

    #[test]
    fn claude_path() {
        let arguments = parse_args(["prog", "/usr/bin/claude"]).unwrap();
        assert_eq!(arguments.claude_argv.len(), 1);
        assert_eq!(arguments.claude_argv[0], "/usr/bin/claude");
    }

    #[test]
    fn claude_path_and_args() {
        let arguments = parse_args(["prog", "/usr/bin/claude", "arg1", "arg2"]).unwrap();
        assert_eq!(arguments.claude_argv.len(), 3);
        assert_eq!(arguments.claude_argv[0], "/usr/bin/claude");
        assert_eq!(arguments.claude_argv[1], "arg1");
        assert_eq!(arguments.claude_argv[2], "arg2");
    }

    #[test]
    fn combined_options() {
        let arguments = parse_args([
            "prog",
            "--say=-v Samantha",
            "--line-wrap=preserve",
            "--",
            "/usr/bin/claude",
            "--some-flag",
        ])
        .unwrap();
        assert!(arguments.say_args.is_some());
        assert_eq!(arguments.line_wrap_mode, LineWrapMode::Preserve);
        assert_eq!(arguments.claude_argv.len(), 2);
        assert_eq!(arguments.claude_argv[0], "/usr/bin/claude");
        assert_eq!(arguments.claude_argv[1], "--some-flag");
    }

    #[test]
    fn unknown_option() {
        let result = parse_args(["prog", "--unknown"]);
        assert!(result.is_err());
    }

    #[test]
    fn input_rewrite_single_rule() {
        let arguments = parse_args(["prog", r"--input-rewrite=\x02:\e[D"]).unwrap();
        assert_eq!(arguments.rewrite_rules.len(), 1);
        assert_eq!(arguments.rewrite_rules[0].from(), b"\x02");
        assert_eq!(arguments.rewrite_rules[0].to(), b"\x1b[D");
    }

    #[test]
    fn input_rewrite_multiple_rules() {
        let arguments = parse_args([
            "prog",
            r"--input-rewrite=\x02:\e[D",
            r"--input-rewrite=\x06:\e[C",
        ])
        .unwrap();
        assert_eq!(arguments.rewrite_rules.len(), 2);
        assert_eq!(arguments.rewrite_rules[0].from(), b"\x02");
        assert_eq!(arguments.rewrite_rules[0].to(), b"\x1b[D");
        assert_eq!(arguments.rewrite_rules[1].from(), b"\x06");
        assert_eq!(arguments.rewrite_rules[1].to(), b"\x1b[C");
    }

    #[test]
    fn input_rewrite_invalid_format() {
        let result = parse_args(["prog", r"--input-rewrite=\x02"]);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("failed to parse --input-rewrite"));
    }

    #[test]
    fn input_rewrite_empty_from() {
        let result = parse_args(["prog", r"--input-rewrite=:\e[D"]);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("failed to parse --input-rewrite"));
    }

    #[test]
    fn input_rewrite_invalid_escape() {
        let result = parse_args(["prog", r"--input-rewrite=\xGG:test"]);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("failed to parse --input-rewrite"));
    }
}
