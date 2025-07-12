#[derive(Debug, PartialEq)]
pub struct Buffer<const N: usize> {
    data: [u8; N],
    start: usize,
    end: usize,
}

impl<const N: usize> Buffer<N> {
    pub fn new() -> Self {
        Self {
            data: [0; N],
            start: 0,
            end: 0,
        }
    }

    pub fn is_full(&self) -> bool {
        self.start == 0 && self.end == N
    }

    pub fn drain(&mut self) -> impl Iterator<Item = Fragment> {
        FragmentIterator {
            full: self.is_full(),
            data: &self.data[..self.end],
            offset: &mut self.start,
        }
    }

    pub fn extend_from_read(&mut self, mut r: impl std::io::Read) -> std::io::Result<usize> {
        if N <= 2 * self.end {
            self.data.copy_within(self.start..self.end, 0);
            self.end -= self.start;
            self.start = 0;
        }
        let n = r.read(&mut self.data[self.end..])?;
        self.end += n;
        Ok(n)
    }
}

#[derive(Debug, PartialEq)]
pub struct Fragment<'a> {
    data: &'a [u8],
    escape_sequence: Option<EscapeSequence<'a>>,
}

impl<'a> Fragment<'a> {
    pub fn new(data: &'a [u8], escape_sequence: Option<EscapeSequence<'a>>) -> Self {
        Self {
            data,
            escape_sequence,
        }
    }

    pub fn data(&self) -> &'a [u8] {
        self.data
    }

    pub fn escape_sequence(&self) -> Option<&EscapeSequence<'a>> {
        self.escape_sequence.as_ref()
    }
}

#[derive(Debug, PartialEq)]
pub enum EscapeSequence<'a> {
    ChangeIconNameAndWindowTitle(&'a [u8]),
    PostNotification(&'a [u8]),
    Incomplete,
    Other,
}

struct FragmentIterator<'a> {
    full: bool,
    data: &'a [u8],
    offset: &'a mut usize,
}

impl<'a> Iterator for FragmentIterator<'a> {
    type Item = Fragment<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        let remaining = &self.data[*self.offset..];
        if remaining.is_empty() {
            return None;
        }

        let offset = *self.offset;
        let mut emit = |consumed, escape_sequence| {
            debug_assert!(0 < consumed);
            debug_assert!(consumed <= remaining.len());
            *self.offset += consumed;
            Some(Fragment::new(
                &self.data[offset..*self.offset],
                escape_sequence,
            ))
        };
        let mut emit_incomplete = || {
            if self.full {
                emit(remaining.len(), Some(EscapeSequence::Incomplete))
            } else {
                None
            }
        };

        match remaining.iter().position(|&b| b == b'\x1b') {
            None => return emit(remaining.len(), None),
            Some(0) if remaining.len() < 2 => return emit_incomplete(),
            Some(0) => (),
            Some(n) => return emit(n, None),
        }

        debug_assert!(remaining.len() >= 2);
        match remaining[1] {
            b'[' => {
                let terminator = remaining[2..]
                    .iter()
                    .position(|&b| (b'\x40'..=b'\x7e').contains(&b))
                    .map(|i| 2 + i + 1);
                let Some(n) = terminator else {
                    return emit_incomplete();
                };
                emit(n, Some(EscapeSequence::Other))
            }
            b'\\' => emit(2, Some(EscapeSequence::Other)),
            b']' => {
                let bel = remaining[2..]
                    .iter()
                    .position(|&b| b == b'\x07')
                    .map(|i| (2 + i, 1));
                let st = || {
                    remaining[2..]
                        .windows(2)
                        .position(|w| w == b"\x1b\\")
                        .map(|i| (2 + i, 2))
                };
                let Some((parameter_end, terminator_length)) = bel.or_else(st) else {
                    return emit_incomplete();
                };
                let n = parameter_end + terminator_length;
                let p = || &remaining[4..parameter_end];
                match &remaining[2..usize::min(4, parameter_end)] {
                    b"0;" => emit(n, Some(EscapeSequence::ChangeIconNameAndWindowTitle(p()))),
                    b"9;" => emit(n, Some(EscapeSequence::PostNotification(p()))),
                    _ => emit(n, Some(EscapeSequence::Other)),
                }
            }
            _ => {
                let n = remaining[2..]
                    .iter()
                    .position(|&b| b == b'\x1b')
                    .map(|i| 2 + i)
                    .unwrap_or(remaining.len());
                emit(n, None)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn change_icon_name_and_window_title_with_bel_terminator() {
        let mut buffer = Buffer::<1024>::new();
        buffer
            .extend_from_read(&b"\x1b]0;Test Title\x07"[..])
            .unwrap();
        assert_eq!(
            buffer.drain().collect::<Vec<_>>(),
            vec![Fragment::new(
                b"\x1b]0;Test Title\x07",
                Some(EscapeSequence::ChangeIconNameAndWindowTitle(b"Test Title")),
            )]
        );
    }

    #[test]
    fn change_icon_name_and_window_title_with_st_terminator() {
        let mut buffer = Buffer::<1024>::new();
        buffer
            .extend_from_read(&b"\x1b]0;Test Title\x1b\\"[..])
            .unwrap();
        assert_eq!(
            buffer.drain().collect::<Vec<_>>(),
            vec![Fragment::new(
                b"\x1b]0;Test Title\x1b\\",
                Some(EscapeSequence::ChangeIconNameAndWindowTitle(b"Test Title")),
            )]
        );
    }

    #[test]
    fn change_icon_name_and_window_title_with_empty_string() {
        let mut buffer = Buffer::<1024>::new();
        buffer.extend_from_read(&b"\x1b]0;\x1b\\"[..]).unwrap();
        assert_eq!(
            buffer.drain().collect::<Vec<_>>(),
            vec![Fragment::new(
                b"\x1b]0;\x1b\\",
                Some(EscapeSequence::ChangeIconNameAndWindowTitle(b"")),
            )]
        );
    }

    #[test]
    fn post_notification_with_bel_terminator() {
        let mut buffer = Buffer::<1024>::new();
        buffer
            .extend_from_read(&b"\x1b]9;Test Message\x07"[..])
            .unwrap();
        assert_eq!(
            buffer.drain().collect::<Vec<_>>(),
            vec![Fragment::new(
                b"\x1b]9;Test Message\x07",
                Some(EscapeSequence::PostNotification(b"Test Message")),
            )]
        );
    }

    #[test]
    fn divided_escape_sequence() {
        let mut buffer = Buffer::<1024>::new();
        buffer
            .extend_from_read(&b"Test Text\x1b]0;Test"[..])
            .unwrap();
        assert_eq!(
            buffer.drain().collect::<Vec<_>>(),
            vec![Fragment::new(b"Test Text", None)],
        );
        buffer.extend_from_read(&b" Title\x07"[..]).unwrap();
        assert_eq!(
            buffer.drain().collect::<Vec<_>>(),
            vec![Fragment::new(
                b"\x1b]0;Test Title\x07",
                Some(EscapeSequence::ChangeIconNameAndWindowTitle(b"Test Title")),
            )]
        );
    }

    #[test]
    fn incomplete_escape_sequence() {
        let mut buffer = Buffer::<17>::new();
        buffer
            .extend_from_read(&b"Test Text\x1b]0;Test"[..])
            .unwrap();
        assert!(buffer.is_full());
        assert_eq!(
            buffer.drain().collect::<Vec<_>>(),
            vec![
                Fragment::new(b"Test Text", None),
                Fragment::new(b"\x1b]0;Test", Some(EscapeSequence::Incomplete)),
            ],
        );
        buffer.extend_from_read(&b" Title\x07"[..]).unwrap();
        assert_eq!(
            buffer.drain().collect::<Vec<_>>(),
            vec![Fragment::new(b" Title\x07", None,)]
        );
    }

    #[test]
    fn other_escape_sequence() {
        let mut buffer = Buffer::<1024>::new();
        buffer
            .extend_from_read(&b"  \x1b[31mTest Text\x1b[0m"[..])
            .unwrap();
        assert_eq!(
            buffer.drain().collect::<Vec<_>>(),
            vec![
                Fragment::new(b"  ", None),
                Fragment::new(b"\x1b[31m", Some(EscapeSequence::Other)),
                Fragment::new(b"Test Text", None),
                Fragment::new(b"\x1b[0m", Some(EscapeSequence::Other)),
            ],
        );
    }
}
