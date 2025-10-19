use std::ops::RangeInclusive;

pub(super) struct FragmentList<'a> {
    inner: Vec<Fragment<'a>>,
}

impl<'a> FragmentList<'a> {
    pub fn parse(mut data: &'a [u8], allow_incomplete: bool) -> Self {
        let mut fragments = vec![];

        while !data.is_empty() {
            let Some(fragment) = Fragment::parse(data, allow_incomplete) else {
                break;
            };
            data = &data[fragment.size()..];
            fragments.push(fragment);
        }

        Self { inner: fragments }
    }

    pub fn into_inner(self) -> Vec<Fragment<'a>> {
        self.inner
    }

    pub fn size(&self) -> usize {
        self.inner.iter().map(|f| f.size()).sum()
    }
}

#[derive(PartialEq)]
pub struct Fragment<'a> {
    data: &'a [u8],
    escape_sequence: Option<EscapeSequence<'a>>,
}

impl<'a> Fragment<'a> {
    fn new(data: &'a [u8], escape_sequence: Option<EscapeSequence<'a>>) -> Self {
        Self {
            data,
            escape_sequence,
        }
    }

    fn parse(data: &'a [u8], allow_incomplete: bool) -> Option<Self> {
        let emit = |consumed, escape_sequence| {
            debug_assert!(0 < consumed);
            debug_assert!(consumed <= data.len());
            Some(Fragment::new(&data[..consumed], escape_sequence))
        };
        let emit_incomplete = || {
            if allow_incomplete {
                emit(data.len(), Some(EscapeSequence::Incomplete))
            } else {
                None
            }
        };

        if data.is_empty() {
            return None;
        }
        match data
            .iter()
            .position(|&b| b == b'\x1b')
            .unwrap_or(data.len())
        {
            0 if data.len() < 2 => emit_incomplete(),
            0 if data[1] == b']' => {
                let bel = data[2..]
                    .iter()
                    .position(|&b| b == b'\x07')
                    .map(|i| (2 + i, 1));
                let st = || {
                    data[2..]
                        .windows(2)
                        .position(|w| w == b"\x1b\\")
                        .map(|i| (2 + i, 2))
                };
                let Some((parameter_end, terminator_length)) = bel.or_else(st) else {
                    return emit_incomplete();
                };
                emit(
                    parameter_end + terminator_length,
                    Some(if data.get(2..4) == Some(b"0;") {
                        EscapeSequence::SetWindowAndIconTitle(&data[4..parameter_end])
                    } else if data.get(2..13) == Some(b"777;notify;")
                        && let title_and_body = &data[13..parameter_end]
                        && let Some(semicolon) = title_and_body.iter().position(|&b| b == b';')
                    {
                        EscapeSequence::ShowDesktopNotification(
                            &title_and_body[..semicolon],
                            &title_and_body[semicolon + 1..],
                        )
                    } else {
                        EscapeSequence::Other
                    }),
                )
            }
            0 => {
                let find_length = |final_bytes: RangeInclusive<u8>| {
                    data[2..]
                        .iter()
                        .position(|b| final_bytes.contains(b))
                        .map(|i| 2 + i + 1)
                };
                let found_length = match data[1] {
                    b'[' => find_length(0x40..=0x7E),
                    0x40..0x5F => Some(2),
                    _ => find_length(0x30..=0x7E),
                };
                let Some(n) = found_length else {
                    return emit_incomplete();
                };
                let escape_sequence = match &data[..n] {
                    b"\x1b[?2026l" => EscapeSequence::EndSynchronizedUpdate,
                    b"\x1b[?25h" => EscapeSequence::ShowCursor,
                    _ => EscapeSequence::Other,
                };
                emit(n, Some(escape_sequence))
            }
            n => emit(
                data.iter()
                    .take(n)
                    .position(|&b| b == b'\n')
                    .map(|i| i + 1)
                    .unwrap_or(n),
                None,
            ),
        }
    }

    pub fn data(&self) -> &'a [u8] {
        self.data
    }

    pub fn size(&self) -> usize {
        self.data.len()
    }

    pub fn escape_sequence(&self) -> Option<&EscapeSequence<'a>> {
        self.escape_sequence.as_ref()
    }
}

#[cfg(feature = "line-wrapping-adjustment")]
impl<'a> Fragment<'a> {
    pub fn is_plain_text(&self) -> bool {
        self.escape_sequence.is_none()
    }

    pub fn chomp(&mut self) {
        let n = self.data.len();
        if n >= 2 && &self.data[n - 2..n] == b"\r\n" {
            self.data = &self.data[..n - 2];
        } else if n >= 1 && self.data[n - 1] == b'\n' {
            self.data = &self.data[..n - 1];
        }
    }

    pub fn ltrim(&mut self) {
        if let Some(i) = self.data.iter().position(|b| *b != b' ') {
            self.data = &self.data[i..];
        } else {
            self.data = &[];
        }
    }
}

impl std::fmt::Debug for Fragment<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Fragment")
            .field("data", &String::from_utf8_lossy(self.data))
            .field("escape_sequence", &self.escape_sequence)
            .finish()
    }
}

#[derive(Debug, PartialEq)]
pub enum EscapeSequence<'a> {
    /// `\x1b[?2026l`
    ///
    /// <https://gist.github.com/christianparpart/d8a62cc1ab659194337d73e399004036>
    EndSynchronizedUpdate,

    /// `\x1b[?25h`
    ///
    /// > ```
    /// > CSI ? Pm h
    /// >           DEC Private Mode Set (DECSET).
    /// >             ...
    /// >             Ps = 2 5  ⇒  Show cursor (DECTCEM), VT220.
    /// > ```
    ///
    /// <https://www.invisible-island.net/xterm/ctlseqs/ctlseqs.html#h4-Functions-using-CSI-_-ordered-by-the-final-character-lparen-s-rparen:CSI-?-Pm-h:Ps-=-2-5.1EC4>
    ShowCursor,

    /// `\x1b]0;title\x07`
    ///
    /// <https://www.invisible-island.net/xterm/ctlseqs/ctlseqs.html#h3-Miscellaneous>
    SetWindowAndIconTitle(&'a [u8]),

    /// `\x1b]777;notify;Title;Body\x07`
    ///
    /// > ```zig
    /// >     const input = "777;notify;Title;Body";
    /// > ```
    ///
    /// <https://github.com/ghostty-org/ghostty/blob/v1.2.2/src/terminal/osc.zig#L2058-L2070>
    ///
    /// Note: caloud adopts OSC 777 to avoid confusion between iTerm2's OSC 9 and ConEmu's OSC 9;1-12.
    ///
    /// - <https://github.com/ghostty-org/ghostty/blob/v1.2.2/src/terminal/osc.zig#L370-L382>
    /// - <https://iterm2.com/documentation-escape-codes.html>
    /// - <https://conemu.github.io/en/AnsiEscapeCodes.html#OSC_Operating_system_commands>
    ShowDesktopNotification(&'a [u8], &'a [u8]),

    Incomplete,

    Other,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn change_icon_name_and_window_title_with_bel_terminator() {
        assert_eq!(
            new_fragments(b"\x1b]0;Test Title\x07", false).into_inner(),
            &[Fragment::new(
                b"\x1b]0;Test Title\x07",
                Some(EscapeSequence::SetWindowAndIconTitle(b"Test Title")),
            )],
        );
    }

    #[test]
    fn change_icon_name_and_window_title_with_st_terminator() {
        assert_eq!(
            new_fragments(b"\x1b]0;Test Title\x1b\\", false).into_inner(),
            &[Fragment::new(
                b"\x1b]0;Test Title\x1b\\",
                Some(EscapeSequence::SetWindowAndIconTitle(b"Test Title")),
            )],
        );
    }

    #[test]
    fn change_icon_name_and_window_title_with_empty_string() {
        assert_eq!(
            new_fragments(b"\x1b]0;\x1b\\", false).into_inner(),
            &[Fragment::new(
                b"\x1b]0;\x1b\\",
                Some(EscapeSequence::SetWindowAndIconTitle(b"")),
            )],
        );
    }

    #[test]
    fn show_desktop_notification_with_bel_terminator() {
        assert_eq!(
            new_fragments(b"\x1b]777;notify;Title;Body\x07", false).into_inner(),
            &[Fragment::new(
                b"\x1b]777;notify;Title;Body\x07",
                Some(EscapeSequence::ShowDesktopNotification(b"Title", b"Body")),
            )],
        );
    }

    #[test]
    fn show_desktop_notification_without_separator() {
        assert_eq!(
            new_fragments(b"\x1b]777;notify;NoSeparator\x07", false).into_inner(),
            &[Fragment::new(
                b"\x1b]777;notify;NoSeparator\x07",
                Some(EscapeSequence::Other),
            )],
        );
    }

    #[test]
    fn allowed_incomplete_escape_sequence() {
        let data = b"Test Text\x1b]0;Test";
        let fragments = FragmentList::parse(data, true);
        assert_eq!(fragments.size(), data.len());
        assert_eq!(
            fragments.into_inner(),
            &[
                Fragment::new(b"Test Text", None),
                Fragment::new(b"\x1b]0;Test", Some(EscapeSequence::Incomplete)),
            ],
        );
    }

    #[test]
    fn disallowed_incomplete_escape_sequence() {
        let data = b"Test Text\x1b]0;Test";
        let fragments = FragmentList::parse(data, false);
        assert_eq!(
            fragments.size(),
            data.iter().position(|&b| b == b'\x1b').unwrap_or(0),
        );
        assert_eq!(fragments.into_inner(), &[Fragment::new(b"Test Text", None)]);
    }

    #[test]
    fn other_escape_sequence() {
        assert_eq!(
            new_fragments(b"\x1b]4;0;#000000\x1b\\", false).into_inner(),
            &[Fragment::new(
                b"\x1b]4;0;#000000\x1b\\",
                Some(EscapeSequence::Other)
            )],
        );
    }

    fn new_fragments(data: &[u8], allow_incomplete: bool) -> FragmentList<'_> {
        let fragments = FragmentList::parse(data, allow_incomplete);
        assert_eq!(fragments.size(), data.len());
        fragments
    }
}
