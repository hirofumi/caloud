use crate::claude::ClaudeCommand;
use crate::macos::say::SayCommand;
use crate::tty_text::reformat::Reformatter;

pub struct Runtime {
    pub notification_center_delivery_enabled: bool,
    pub say_command: Option<SayCommand>,
    pub reformatter: Reformatter,
    pub claude_command: ClaudeCommand,
}
