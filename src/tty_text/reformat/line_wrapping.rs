use crate::tty_text::fragment::{EscapeSequence, Fragment};

const MAX_CONTINUATION_INDENT: usize = 2;
const WRAP_EDGE_SLACK: usize = 4;

pub(super) fn adjust_line_wrapping(
    fragments: &mut Vec<Fragment>,
    allow_incomplete: bool,
    terminal_width: u16,
) -> usize {
    let mut cursor = FragmentCursor::new(fragments, terminal_width);
    let mut prev: Option<(usize, Vec<u8>)> = None;

    'outer: loop {
        let mut adjusted = cursor.position();
        let Some(mut line) = cursor.extract_line() else {
            break;
        };

        // Try backward join before the URL check: the current line may lack
        // `://` entirely when the split falls inside the `://` itself
        // (e.g. `file:` on line A, `///...` on line B).
        if let Some((prev_adjusted, prev_line)) = prev.take()
            && has_split_url_scheme(&prev_line, &line)
        {
            cursor.join();
            cursor.set_position(prev_adjusted);
            adjusted = prev_adjusted;
            // join() removes only the A-B boundary; B's trailing boundary remains.
            line = cursor.extract_line().unwrap();
        }

        if !should_attempt_url_unwrap(&line, terminal_width) {
            prev = (line_width(&line) == usize::from(terminal_width)).then_some((adjusted, line));
            continue;
        }
        prev = None;

        let mut previous_line_width = line_width(&line);

        loop {
            let Some(line) = cursor.extract_line() else {
                cursor.set_position(adjusted);
                break 'outer;
            };

            let Some(margin) = url_continuation_indent(&line) else {
                cursor.rewind();
                break;
            };

            // Only join continuations whose content is all ASCII graphic (no spaces).
            // Lines with spaces are surrounding prose, not URL fragments; joining
            // them would lose the inter-word space consumed by terminal wrapping.
            if !is_ascii_graphic_run(&line[margin..]) {
                // Exception: if the previous line filled exactly terminal_width
                // (forced wrap at the boundary), this is likely a URL split.
                // Cursor-forward escapes within the continuation preserve word-boundary
                // spaces, so joining the whole line is safe.
                if usize::from(terminal_width) > previous_line_width {
                    cursor.rewind();
                    break;
                }
            }

            cursor.join();

            previous_line_width = line_width(&line);

            if !can_have_another_url_continuation(&line, terminal_width) {
                cursor.rewind();
                break;
            }
        }
    }

    if allow_incomplete && cursor.position() == 0 && cursor.len() > 1 {
        cursor.set_position(1);
    }

    cursor.truncate()
}

struct FragmentCursor<'a, 'b> {
    fragments: &'b mut Vec<Fragment<'a>>,
    end_offsets: Vec<usize>,
    position: usize,
    line_start: usize,
    terminal_width: usize,
}

impl<'a, 'b> FragmentCursor<'a, 'b> {
    fn new(fragments: &'b mut Vec<Fragment<'a>>, terminal_width: u16) -> Self {
        let end_offsets = fragments
            .iter()
            .scan(0, |acc, f| {
                *acc += f.size();
                Some(*acc)
            })
            .collect();
        Self {
            fragments,
            end_offsets,
            position: 0,
            line_start: 0,
            terminal_width: terminal_width.into(),
        }
    }

    fn position(&self) -> usize {
        self.position
    }

    fn set_position(&mut self, pos: usize) {
        self.position = pos;
    }

    fn len(&self) -> usize {
        self.fragments.len()
    }

    fn rewind(&mut self) {
        self.position = self.line_start;
    }

    fn remove_at(&mut self, index: usize) {
        self.fragments.remove(index);
        self.end_offsets.remove(index);
        self.position -= 1;
    }

    fn remove_while(&mut self, index: usize, predicate: impl Fn(&Fragment) -> bool) {
        while self.fragments.get(index).is_some_and(&predicate) {
            self.remove_at(index);
        }
    }

    fn truncate(self) -> usize {
        self.fragments.truncate(self.position);
        self.end_offsets
            .get(self.position.wrapping_sub(1))
            .copied()
            .unwrap_or(0)
    }

    fn extract_line(&mut self) -> Option<Vec<u8>> {
        let start = self.position;
        let mut i = self.position;
        let mut found = false;

        while let Some(fragment) = self.fragments.get(i) {
            i += 1;
            match fragment.escape_sequence() {
                None => {
                    if fragment.data().contains(&b'\n')
                        || visual_line_break_length(&self.fragments[i - 1..]).is_some()
                    {
                        found = true;
                        break;
                    }
                }
                Some(
                    EscapeSequence::EndSynchronizedUpdate
                    | EscapeSequence::ShowCursor
                    | EscapeSequence::SetWindowAndIconTitle(_)
                    | EscapeSequence::PostNotification(_),
                ) => {
                    found = true;
                    break;
                }
                Some(EscapeSequence::Incomplete | EscapeSequence::Other) => {}
            }
        }

        if !found {
            return None;
        }

        self.line_start = start;
        self.position = i;

        // Flatten fragments into a text byte sequence for line analysis.
        // CUF escapes are expanded to spaces so that width/margin calculations
        // in adjust_line_wrapping see the correct column positions.
        let mut line = Vec::new();
        for f in &self.fragments[start..self.position] {
            if f.is_plain_text() {
                line.extend_from_slice(f.data());
            } else if let Some(n) = cursor_forward_columns(f.data()) {
                line.extend(std::iter::repeat_n(b' ', n.min(self.terminal_width)));
            }
        }
        if line.last() == Some(&b'\n') {
            line.pop();
        }
        while line.last() == Some(&b'\r') {
            line.pop();
        }
        Some(line)
    }

    /// Join the current continuation line with the previous line.
    ///
    /// Removes the line boundary `(\n | \r CUF? CUD) CUF*` and
    /// left-trims the first plain-text fragment of the continuation.
    fn join(&mut self) {
        debug_assert!(self.line_start >= 1);
        let visual_break_length = visual_line_break_length(&self.fragments[self.line_start - 1..]);
        self.fragments[self.line_start - 1].chomp();

        let mut i = self.line_start;
        // Remove the CUF? CUD fragments identified by visual_line_break_length.
        // `visual_break_length` counts from the \r fragment (index 0) up to and
        // including the CUD, so fragments 1..visual_break_length are CUF?/CUD.
        for _ in 1..visual_break_length.unwrap_or(0) {
            self.remove_at(i);
        }
        self.remove_while(i, |f| f.is_cuf());
        while i < self.position {
            if self.fragments[i].is_plain_text() {
                self.fragments[i].ltrim();
                break;
            }
            i += 1;
        }
    }
}

/// Returns the number of fragments that form a visual line break (`\r CUF? CUD`),
/// or `None` if the fragments do not start with one.
fn visual_line_break_length(fragments: &[Fragment]) -> Option<usize> {
    if !fragments.first().is_some_and(|f| f.data().ends_with(b"\r")) {
        return None;
    }
    let mut i = 1;
    if fragments.get(i).is_some_and(Fragment::is_cuf) {
        i += 1;
    }
    fragments
        .get(i)
        .is_some_and(Fragment::is_cud)
        .then_some(i + 1)
}

fn cursor_forward_columns(data: &[u8]) -> Option<usize> {
    let data = data.strip_prefix(b"\x1b[")?;
    let data = data.strip_suffix(b"C")?;
    // > NOTE: Pn is a variable, ASCII coded, numeric parameter.
    // > If you select no parameter or a parameter value of 0,
    // > the terminal assumes the parameter equals 1.
    // https://vt100.net/docs/vt220-rm/chapter4.html#S4.7
    let n = if data.is_empty() {
        1
    } else {
        let v = std::str::from_utf8(data).ok()?.parse().ok()?;
        if v == 0 { 1 } else { v }
    };
    Some(n)
}

fn should_attempt_url_unwrap(line: &[u8], terminal_width: u16) -> bool {
    const MARKER: &[u8] = b"://";

    let Some(i) = line.windows(MARKER.len()).rposition(|w| w == MARKER) else {
        return false;
    };

    let i = i + MARKER.len();

    if line[i..].iter().any(|&b| !b.is_ascii_graphic()) {
        return false;
    }

    usize::from(terminal_width).saturating_sub(WRAP_EDGE_SLACK)
        <= std::str::from_utf8(&line[..i])
            .map(unicode_width::UnicodeWidthStr::width)
            .unwrap_or(i)
            + line[i..].len()
}

fn url_continuation_indent(line: &[u8]) -> Option<usize> {
    let margin = continuation_indent(line)?;

    // ordered list
    if let Some(i) = line[margin..].iter().position(|&b| !b.is_ascii_digit())
        && i > 0
    {
        let i = margin + i;
        if line.get(i) == Some(&b'.') && line.get(i + 1).is_none_or(|&b| b == b' ') {
            return None;
        }
    }

    // unordered list
    if line.get(margin) == Some(&b'-') && line.get(margin + 1).is_none_or(|&b| b == b' ') {
        return None;
    }

    if starts_with_url_scheme(&line[margin..]) {
        return None;
    }

    line[margin].is_ascii_graphic().then_some(margin)
}

fn can_have_another_url_continuation(line: &[u8], terminal_width: u16) -> bool {
    let Some(margin) = continuation_indent(line) else {
        return false;
    };

    usize::from(terminal_width).saturating_sub(WRAP_EDGE_SLACK) <= line.len()
        && is_ascii_graphic_run(&line[margin..])
}

fn continuation_indent(line: &[u8]) -> Option<usize> {
    let n = line.iter().take_while(|&&b| b == b' ').count();
    (1..=MAX_CONTINUATION_INDENT)
        .contains(&n)
        .then_some(n)
        .filter(|&n| n < line.len())
}

fn starts_with_url_scheme(line: &[u8]) -> bool {
    line.iter()
        .position(|&b| !is_scheme_char(b))
        .is_some_and(|i| line[0].is_ascii_alphabetic() && line[i..].starts_with(b"://"))
}

/// Detect a URL scheme split across a forced line wrap.
///
/// When inline code containing a URL is styled with SGR escapes and the
/// terminal forces a wrap at the column boundary, the scheme can end up
/// split.  After SGR stripping by `extract_line`, neither line alone
/// contains a recognisable `scheme://` pattern.
///
/// Two split positions are handled:
///
/// - **Intra-scheme**: scheme characters are split across lines.
///   E.g. `fil` on line A and `e:///...` on line B.
///
/// - **Colon boundary**: the colon lands at the end of line A and `//`
///   starts line B.  E.g. `file:` on line A and `///...` on line B.
fn has_split_url_scheme(prev_line: &[u8], current_line: &[u8]) -> bool {
    let Some(margin) = continuation_indent(current_line) else {
        return false;
    };
    let after_margin = &current_line[margin..];

    // Intra-scheme split: `://` is on the current line but the leading
    // scheme characters are incomplete (the rest is on prev_line).
    //
    // Known false-positive risk: if prev_line ends with a word that
    // happens to consist of valid scheme chars (e.g. "file") and the
    // current line starts with a complete URL (e.g. "https://..."), the
    // two are incorrectly joined.  This requires: (a) prev at terminal
    // width, (b) 1-2 space continuation indent, and (c) a URL
    // immediately after indent.
    if let Some(colon_pos) = after_margin.windows(3).position(|w| w == b"://")
        && after_margin[..colon_pos].iter().all(|&b| is_scheme_char(b))
    {
        let prefix_len = prev_line
            .iter()
            .rev()
            .take_while(|&&b| is_scheme_char(b))
            .count();
        if prefix_len > 0 && prev_line[prev_line.len() - prefix_len].is_ascii_alphabetic() {
            return true;
        }
    }

    // Colon-boundary split: prev_line ends with `<scheme>:` and the current
    // line begins with `//`.
    if after_margin.starts_with(b"//") && prev_line.last() == Some(&b':') {
        let before_colon = &prev_line[..prev_line.len() - 1];
        let scheme_len = before_colon
            .iter()
            .rev()
            .take_while(|&&b| is_scheme_char(b))
            .count();
        if scheme_len > 0 && before_colon[before_colon.len() - scheme_len].is_ascii_alphabetic() {
            return true;
        }
    }

    false
}

/// RFC 3986 §3.1: `scheme = ALPHA *( ALPHA / DIGIT / "+" / "-" / "." )`
fn is_scheme_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.')
}

fn is_ascii_graphic_run(data: &[u8]) -> bool {
    data.iter().all(|&b| b.is_ascii_graphic())
}

fn line_width(line: &[u8]) -> usize {
    std::str::from_utf8(line)
        .map(unicode_width::UnicodeWidthStr::width)
        .unwrap_or(line.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tty_text::fragment::FragmentList;
    use proptest::prelude::*;
    use proptest::property_test;

    #[test]
    fn snapshot() {
        // Binary snapshots to preserve \r in visual line breaks (\r CUF? CUD).
        // Text snapshots would normalize \r away on storage.
        insta::glob!("snapshots/*.capture.raw", |path| {
            let stem = path.file_stem().unwrap().to_str().unwrap();
            let name = stem.strip_suffix(".capture").unwrap();
            let data = std::fs::read(path).unwrap();
            let mut fragments = FragmentList::parse(&data, false).into_inner();
            adjust_line_wrapping(&mut fragments, false, 40); // column width set in capture.exp
            let output: Vec<u8> = fragments.iter().flat_map(|f| f.data()).copied().collect();
            let mut settings = insta::Settings::clone_current();
            settings.set_prepend_module_to_snapshot(false);
            settings.remove_snapshot_suffix();
            settings.bind(|| {
                let snap_name = format!("{name}.raw");
                insta::assert_binary_snapshot!(&snap_name, output);
            });
        });
    }

    #[property_test]
    fn consumes_all_when_no_pending_url(
        #[strategy = arb_pty_input_without_pending_url()] (tw, data): (u16, Vec<u8>),
    ) {
        let mut fragments = FragmentList::parse(&data, false).into_inner();
        let consumed = adjust_line_wrapping(&mut fragments, false, tw);
        prop_assert_eq!(consumed, data.len());
    }

    #[property_test]
    fn consumes_up_to_pending_url(
        #[strategy = arb_pty_input_with_pending_url()] (tw, data, pending_length): (
            u16,
            Vec<u8>,
            usize,
        ),
    ) {
        let mut fragments = FragmentList::parse(&data, false).into_inner();
        let consumed = adjust_line_wrapping(&mut fragments, false, tw);
        prop_assert_eq!(consumed + pending_length, data.len());
    }

    fn arb_pty_input_without_pending_url() -> impl Strategy<Value = (u16, Vec<u8>)> {
        (10u16..=200, prop::bool::ANY)
            .prop_flat_map(|(tw, trailing)| {
                (
                    Just(tw),
                    prop::collection::vec(arb_line_group(tw, true), 0..4),
                    arb_line_group(tw, trailing),
                )
            })
            .prop_map(move |(tw, groups, last)| {
                let mut data: Vec<u8> = groups.concat();
                data.extend_from_slice(&last);
                // Append an empty line so the last URL (if any) is not pending.
                data.extend_from_slice(b"\n\n");
                (tw, data)
            })
    }

    fn arb_pty_input_with_pending_url() -> impl Strategy<Value = (u16, Vec<u8>, usize)> {
        (10u16..=200, prop::bool::ANY)
            .prop_flat_map(|(tw, trailing)| {
                (
                    Just(tw),
                    prop::collection::vec(arb_line_group(tw, true), 0..4),
                    arb_wrapped_url(tw, trailing),
                )
            })
            .prop_map(|(tw, groups, last)| {
                let mut data: Vec<u8> = groups.concat();
                // Ensure data ends with \n so the pending URL is on its
                // own line.  Visual breaks leave CUF/CUD fragments that would
                // be grouped with the pending URL by extract_line.
                if !data.is_empty() && !data.ends_with(b"\n") {
                    data.push(b'\n');
                }
                data.extend_from_slice(&last);
                (tw, data, last.len())
            })
    }

    fn arb_line_group(tw: u16, trailing: bool) -> impl Strategy<Value = Vec<u8>> {
        prop_oneof![arb_single_line(tw, trailing), arb_wrapped_url(tw, trailing)]
    }

    fn arb_single_line(tw: u16, trailing: bool) -> impl Strategy<Value = Vec<u8>> {
        (
            prop::collection::vec(0x20u8..=0x7e, 0..(tw as usize)),
            arb_break_and_indent(0),
        )
            .prop_map(move |(mut line, brk)| {
                if trailing {
                    line.extend_from_slice(&brk);
                }
                line
            })
    }

    fn arb_wrapped_url(tw: u16, trailing: bool) -> impl Strategy<Value = Vec<u8>> {
        let tw = tw as usize;
        let min_url_length = "a://b".len();

        (
            min_url_length..=tw,
            0usize..4,
            1usize..=MAX_CONTINUATION_INDENT,
        )
            .prop_flat_map(move |(url_length, n, indent)| {
                (
                    arb_url_body_without_colon(tw - url_length),
                    arb_url(url_length),
                    prop::collection::vec(
                        (
                            arb_break_and_indent(indent),
                            arb_url_body_without_colon(tw - indent),
                        ),
                        n,
                    ),
                    if trailing {
                        (arb_break_and_indent(0), Just(vec![])).boxed()
                    } else {
                        (0..=tw - indent)
                            .prop_flat_map(move |n| {
                                (arb_break_and_indent(indent), arb_url_body_without_colon(n))
                            })
                            .boxed()
                    },
                )
            })
            .prop_map(move |(prefix, url, continuations, last_continuation)| {
                let mut out = prefix;
                out.extend_from_slice(&url);
                for (break_and_indent, content) in continuations
                    .iter()
                    .chain(std::iter::once(&last_continuation))
                {
                    out.extend_from_slice(break_and_indent);
                    out.extend_from_slice(content);
                }
                out
            })
    }

    /// URL of exactly `len` bytes (`scheme :// ...`).
    /// Rejects if `len` is too short to hold `scheme://`.
    fn arb_url(len: usize) -> impl Strategy<Value = Vec<u8>> {
        let max_scheme = len - "://".len();
        arb_scheme(max_scheme).prop_flat_map(move |scheme| {
            let content_len = len - scheme.len() - "://".len();
            arb_url_body_without_colon(content_len).prop_map(move |content| {
                let mut url = scheme.clone();
                url.extend_from_slice(b"://");
                url.extend_from_slice(&content);
                url
            })
        })
    }

    /// ASCII graphic bytes excluding `:` (0x3A).
    ///
    /// Prevents accidental `://` sequences that would mislead
    /// `should_attempt_url_unwrap`'s `rposition` search or trigger
    /// `starts_with_url_scheme` on continuation lines.
    fn arb_url_body_without_colon(
        length: impl Into<prop::collection::SizeRange>,
    ) -> impl Strategy<Value = Vec<u8>> {
        prop::collection::vec((0x21u8..0x3A).prop_union(0x3Bu8..0x7F), length)
    }

    /// URL scheme: 1–`max_length` lowercase ASCII letters.
    ///
    /// `should_attempt_url_unwrap` recognises schemes matching RFC 3986 §3.1
    /// (`ALPHA *( ALPHA / DIGIT / "+" / "-" / "." ) "://"`) via
    /// `starts_with_url_scheme`.  Lowercase letters cover the common
    /// case and are sufficient to exercise all code paths.
    fn arb_scheme(max_length: usize) -> impl Strategy<Value = Vec<u8>> {
        prop::collection::vec(b'a'..=b'z', 1..=max_length)
    }

    /// Generate bytes for a line break followed by `indent` columns of indentation.
    fn arb_break_and_indent(indent: usize) -> impl Strategy<Value = Vec<u8>> {
        let cud = || b"\x1b[1B".to_vec();
        let cuf = |n: usize| {
            (n == 0)
                .then_some(vec![])
                .unwrap_or_else(|| format!("\x1b[{n}C").into_bytes())
        };

        prop_oneof![
            Just([b"\n".to_vec(), vec![b' '; indent]].concat()),
            Just([b"\n".to_vec(), cuf(indent)].concat()),
            Just([b"\r".to_vec(), cud(), vec![b' '; indent]].concat()),
            Just([b"\r".to_vec(), cud(), cuf(indent)].concat()),
            Just([b"\r\r".to_vec(), cud(), vec![b' '; indent]].concat()),
            Just([b"\r\r".to_vec(), cud(), cuf(indent)].concat()),
            (0..=indent).prop_map(move |pre| {
                [b"\r".to_vec(), cuf(pre), cud(), cuf(indent - pre)].concat()
            }),
        ]
    }
}
