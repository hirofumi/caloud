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

        if data.is_empty() {
            return None;
        }
        match data
            .iter()
            .position(|&b| b == b'\x1b')
            .unwrap_or(data.len())
        {
            0 => {
                let (consumed, escape_sequence) = EscapeSequence::parse(data, allow_incomplete)?;
                emit(consumed, Some(escape_sequence))
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

impl<'a> Fragment<'a> {
    pub fn is_plain_text(&self) -> bool {
        self.escape_sequence.is_none()
    }

    /// CSI Ps B — Cursor Down (CUD)
    pub(super) fn is_cud(&self) -> bool {
        self.data.len() >= 3
            && self.data.starts_with(b"\x1b[")
            && self.data[2..self.data.len() - 1]
                .iter()
                .all(|b| b.is_ascii_digit())
            && self.data[self.data.len() - 1] == b'B'
    }

    /// CSI Ps C — Cursor Forward (CUF)
    pub(super) fn is_cuf(&self) -> bool {
        self.data.len() >= 3
            && self.data.starts_with(b"\x1b[")
            && self.data[2..self.data.len() - 1]
                .iter()
                .all(|b| b.is_ascii_digit())
            && self.data[self.data.len() - 1] == b'C'
    }

    pub(super) fn chomp(&mut self) {
        if let Some(rest) = self.data.strip_suffix(b"\n") {
            self.data = rest;
        }
        while let Some(rest) = self.data.strip_suffix(b"\r") {
            self.data = rest;
        }
    }

    pub(super) fn ltrim(&mut self) {
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

    /// `\x1b]9;message\x07`
    ///
    /// > To post a notification:
    /// >
    /// > ```
    /// > OSC 9 ; [Message content goes here] ST
    /// > ```
    ///
    /// <https://iterm2.com/documentation-escape-codes.html>
    PostNotification(&'a [u8]),

    Incomplete,

    Other,
}

impl<'a> EscapeSequence<'a> {
    fn parse(data: &'a [u8], allow_incomplete: bool) -> Option<(usize, Self)> {
        if data.first() != Some(&b'\x1b') {
            return None;
        }

        let emit_incomplete = || {
            if allow_incomplete {
                Some((data.len(), EscapeSequence::Incomplete))
            } else {
                None
            }
        };

        if data.len() < 2 {
            return emit_incomplete();
        }

        if data[1] == b']' {
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
            let p = || &data[4..parameter_end];
            let has_conemu_osc9_parameter = || {
                // https://conemu.github.io/en/AnsiEscapeCodes.html#ConEmu_specific_OSC
                matches!(p(), b"5" | b"10" | b"12")
                    || p()
                        .splitn(2, |b| *b == b';')
                        .next()
                        .is_some_and(|s| s.iter().all(|b| b.is_ascii_digit()))
            };
            return Some((
                parameter_end + terminator_length,
                match &data[2..usize::min(4, parameter_end)] {
                    b"0;" => EscapeSequence::SetWindowAndIconTitle(p()),
                    b"9;" if !has_conemu_osc9_parameter() => EscapeSequence::PostNotification(p()),
                    _ => EscapeSequence::Other,
                },
            ));
        }

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
        Some((n, escape_sequence))
    }
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
    fn post_notification_with_bel_terminator() {
        assert_eq!(
            new_fragments(b"\x1b]9;Test Message\x07", false).into_inner(),
            &[Fragment::new(
                b"\x1b]9;Test Message\x07",
                Some(EscapeSequence::PostNotification(b"Test Message")),
            )],
        );
    }

    #[test]
    fn conemu_set_progress_state() {
        assert_eq!(
            new_fragments(b"\x1b]9;4;1;50\x07", false).into_inner(),
            &[Fragment::new(
                b"\x1b]9;4;1;50\x07",
                Some(EscapeSequence::Other)
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
