mod line_wrapping;

use crate::tty_text::fragment::{Fragment, FragmentList};
use line_wrapping::adjust_line_wrapping;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineWrapMode {
    Preserve,
    Adjust,
}

#[derive(Debug)]
pub struct Reformatter {
    terminal_width: u16,
    mode: LineWrapMode,
}

impl Reformatter {
    pub fn new(terminal_width: u16, mode: LineWrapMode) -> Self {
        Self {
            terminal_width,
            mode,
        }
    }

    pub fn set_terminal_width(&mut self, terminal_width: u16) {
        self.terminal_width = terminal_width;
    }

    pub(super) fn reformat<'a>(
        &self,
        fragments: FragmentList<'a>,
        is_full: bool,
    ) -> (usize, Vec<Fragment<'a>>) {
        match self.mode {
            LineWrapMode::Adjust => {
                let mut fragments = fragments.into_inner();
                let consumed = adjust_line_wrapping(&mut fragments, is_full, self.terminal_width);
                (consumed, fragments)
            }
            LineWrapMode::Preserve => {
                let consumed = fragments.size();
                (consumed, fragments.into_inner())
            }
        }
    }
}
