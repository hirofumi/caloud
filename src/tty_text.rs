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

    pub fn drain(&mut self) -> FragmentIterator<'_> {
        FragmentIterator {
            full: self.is_full(),
            data: &self.data[..self.end],
            offset: &mut self.start,
            previous_offset: None,
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
    osc: Option<Osc<'a>>,
}

impl<'a> Fragment<'a> {
    pub fn new(data: &'a [u8], osc: Option<Osc<'a>>) -> Self {
        Self { data, osc }
    }

    pub fn data(&self) -> &'a [u8] {
        self.data
    }

    pub fn osc(&self) -> Option<&Osc<'a>> {
        self.osc.as_ref()
    }
}

#[derive(Debug, PartialEq)]
pub enum Osc<'a> {
    ChangeIconNameAndWindowTitle(&'a [u8]),
    PostNotification(&'a [u8]),
    Incomplete,
    Other,
}

pub struct FragmentIterator<'a> {
    full: bool,
    data: &'a [u8],
    offset: &'a mut usize,
    previous_offset: Option<usize>,
}

#[cfg(feature = "line-wrapping-adjustment")]
impl<'a> FragmentIterator<'a> {
    pub fn adjust_line_wrapping(self) -> impl Iterator<Item = Fragment<'a>> {
        LineWrappingAdjuster::new(self)
    }

    fn has_next(&self) -> bool {
        *self.offset < self.data.len()
    }

    fn step_back(&mut self) -> bool {
        if let Some(previous_offset) = self.previous_offset.take() {
            *self.offset = previous_offset;
            true
        } else {
            false
        }
    }
}

impl<'a> Iterator for FragmentIterator<'a> {
    type Item = Fragment<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        let remaining = &self.data[*self.offset..];
        if remaining.is_empty() {
            return None;
        }

        let offset = *self.offset;
        let mut emit = |consumed, osc| {
            debug_assert!(0 < consumed);
            debug_assert!(consumed <= remaining.len());
            self.previous_offset = Some(*self.offset);
            *self.offset += consumed;
            Some(Fragment::new(&self.data[offset..*self.offset], osc))
        };
        let mut emit_incomplete = || {
            if self.full {
                emit(remaining.len(), Some(Osc::Incomplete))
            } else {
                None
            }
        };

        match remaining.windows(2).position(|w| w == b"\x1b]") {
            None => return emit(remaining.len(), None),
            Some(0) if remaining.len() < 2 => return emit_incomplete(),
            Some(0) => (),
            Some(n) => return emit(n, None),
        }

        debug_assert!(remaining.len() >= 2);
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
            b"0;" => emit(n, Some(Osc::ChangeIconNameAndWindowTitle(p()))),
            b"9;" => emit(n, Some(Osc::PostNotification(p()))),
            _ => emit(n, Some(Osc::Other)),
        }
    }
}

#[cfg(feature = "line-wrapping-adjustment")]
struct LineWrappingAdjuster<'a> {
    inner: FragmentIterator<'a>,
    deferred: std::collections::VecDeque<Fragment<'a>>,
}

#[cfg(feature = "line-wrapping-adjustment")]
impl<'a> LineWrappingAdjuster<'a> {
    fn new(inner: FragmentIterator<'a>) -> Self {
        Self {
            inner,
            deferred: std::collections::VecDeque::new(),
        }
    }
}

#[cfg(feature = "line-wrapping-adjustment")]
impl<'a> Iterator for LineWrappingAdjuster<'a> {
    type Item = Fragment<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(fragment) = self.deferred.pop_front() {
            return Some(fragment);
        }

        let fragment = self.inner.next()?;

        if fragment.osc().is_some() {
            return Some(fragment);
        }

        let mut data = fragment.data();
        let mut offset = 0;
        loop {
            let Some(i) = data[offset..].windows(3).position(|w| w == b"://") else {
                self.deferred.push_back(Fragment::new(data, None));
                break;
            };
            if data[usize::max(0, offset + i - 4)..offset + i] != b"file"[..]
                && data[usize::max(0, offset + i - 4)..offset + i] != b"http"[..]
                && data[usize::max(0, offset + i - 5)..offset + i] != b"https"[..]
            {
                offset += i + 3;
                continue;
            }
            self.deferred
                .push_back(Fragment::new(&data[offset..offset + i + 3], None));
            offset += i + 3;
            let mut gap_fragment = None;
            loop {
                let url_break = data[offset..]
                    .iter()
                    .position(|&b| !b.is_ascii_graphic())
                    .map(|i| offset + i)
                    .unwrap_or(data.len());
                let mut url_cont = url_break;
                let mut linebreak_found = false;
                while url_cont < data.len() {
                    if data[url_cont..].starts_with(b"\x1b[") {
                        url_cont += 2;
                        while url_cont < data.len() {
                            if (b'\x40'..=b'\x7e').contains(&data[url_cont]) {
                                url_cont += 1;
                                break;
                            }
                            url_cont += 1;
                        }
                        continue;
                    } else if !linebreak_found && data[url_cont..].starts_with(b"\r\n  ") {
                        linebreak_found = true;
                        url_cont += 4;
                        continue;
                    } else {
                        break;
                    }
                }
                self.deferred
                    .push_back(Fragment::new(&data[offset..url_break], None));
                offset = url_cont;
                if url_cont == url_break {
                    break;
                }
                gap_fragment = Some(Fragment::new(&data[url_break..url_cont], None));
                if data[offset..].is_empty()
                    || !data[offset].is_ascii_graphic()
                    || data[offset..].starts_with(b"- ")
                    || data[offset..].starts_with(b"-\r")
                {
                    break;
                }
            }
            if offset == data.len() && !self.inner.has_next() && self.inner.step_back() {
                return None;
            }
            self.deferred.extend(gap_fragment);
            data = &data[offset..];
            offset = 0;
        }

        self.deferred.pop_front()
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
                Some(Osc::ChangeIconNameAndWindowTitle(b"Test Title")),
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
                Some(Osc::ChangeIconNameAndWindowTitle(b"Test Title")),
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
                Some(Osc::ChangeIconNameAndWindowTitle(b"")),
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
                Some(Osc::PostNotification(b"Test Message")),
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
                Some(Osc::ChangeIconNameAndWindowTitle(b"Test Title")),
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
                Fragment::new(b"\x1b]0;Test", Some(Osc::Incomplete)),
            ],
        );
        buffer.extend_from_read(&b" Title\x07"[..]).unwrap();
        assert_eq!(
            buffer.drain().collect::<Vec<_>>(),
            vec![Fragment::new(b" Title\x07", None)]
        );
    }

    #[test]
    fn other_escape_sequence() {
        let mut buffer = Buffer::<1024>::new();
        buffer
            .extend_from_read(&b"\x1b]4;0;#000000\x1b\\"[..])
            .unwrap();
        assert_eq!(
            buffer.drain().collect::<Vec<_>>(),
            vec![Fragment::new(b"\x1b]4;0;#000000\x1b\\", Some(Osc::Other))],
        );
    }

    #[cfg(feature = "line-wrapping-adjustment")]
    #[test]
    fn adjust_line_wrapping_for_long_url() {
        let mut buffer = Buffer::<1024>::new();
        buffer
            .extend_from_read(
                [
                    &b"\x1b[38;5;231m\xE2\x8F\xBA\x1b[39m \x1b[38;5;153mURL: https://example.c"[..],
                    &b"  om/long/long/path/to/re"[..],
                    &b"  source and some more text."[..],
                ]
                .join(&b"\r\n"[..])
                .as_slice(),
            )
            .unwrap();
        assert_eq!(
            buffer.drain().adjust_line_wrapping().collect::<Vec<_>>(),
            vec![
                Fragment::new(
                    b"\x1b[38;5;231m\xE2\x8F\xBA\x1b[39m \x1b[38;5;153mURL: https://",
                    None
                ),
                Fragment::new(b"example.c", None),
                Fragment::new(b"om/long/long/path/to/re", None),
                Fragment::new(b"source", None),
                Fragment::new(b"\r\n  ", None),
                Fragment::new(b" and some more text.", None),
            ]
        );
    }

    #[cfg(feature = "line-wrapping-adjustment")]
    #[test]
    fn adjust_line_wrapping_for_unordered_list() {
        let mut buffer = Buffer::<1024>::new();
        buffer
            .extend_from_read(
                [
                    &b"  - https://example.c"[..],
                    &b"  om/1"[..],
                    &b"  - https://example.c"[..],
                    &b"  om/2"[..],
                    &b"  -"[..],
                    &b"  https://example.c"[..],
                    &b"  om/3"[..],
                    &b"  - https://example.c"[..],
                    &b"  om/4"[..],
                    &b"  "[..],
                    &b"  some more text"[..],
                ]
                .join(&b"\r\n"[..])
                .as_slice(),
            )
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(
                &buffer
                    .drain()
                    .adjust_line_wrapping()
                    .map(|f| f.data)
                    .collect::<Vec<_>>()
                    .concat(),
            ),
            String::from_utf8_lossy(
                &[
                    &b"  - https://example.com/1"[..],
                    &b"  - https://example.com/2"[..],
                    &b"  -"[..],
                    &b"  https://example.com/3"[..],
                    &b"  - https://example.com/4"[..],
                    &b"  "[..],
                    &b"  some more text"[..],
                ]
                .join(&b"\r\n"[..]),
            ),
        );
    }
}
