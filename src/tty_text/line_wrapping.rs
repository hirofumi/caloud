#![cfg(feature = "line-wrapping-adjustment")]

use crate::tty_text::fragment::{EscapeSequence, Fragment};

const INDENT: &[u8] = b"  ";
const LEFT_MARGIN: usize = INDENT.len();
const RIGHT_MARGIN: usize = 4;

pub(super) fn adjust_line_wrapping(
    fragments: &mut Vec<Fragment>,
    allow_incomplete: bool,
    terminal_width: u16,
) -> usize {
    let original_sizes = fragments.iter().map(|f| f.size()).collect::<Vec<_>>();

    let mut index = 0;
    'outer: while index < fragments.len() {
        let adjusted = index;
        let Some(line) = extract_line(fragments, &mut index) else {
            break;
        };
        debug_assert!(fragments[index - 1].is_plain_text());

        if !can_be_wrapped(&line, terminal_width) {
            continue;
        }

        loop {
            let maybe_wrapped = index;
            let Some(line) = extract_line(fragments, &mut index) else {
                index = adjusted;
                break 'outer;
            };

            if !can_follow_wrapped(&line) {
                index = maybe_wrapped;
                break;
            }

            fragments[maybe_wrapped - 1].chomp();
            for fragment in &mut fragments[maybe_wrapped..index] {
                if fragment.is_plain_text() {
                    fragment.ltrim();
                    break;
                }
            }

            if !can_be_wrapped_again(&line, terminal_width) {
                index = maybe_wrapped;
                break;
            }
        }
    }
    if allow_incomplete && index == 0 && fragments.len() > 1 {
        index = 1;
    }

    fragments.truncate(index);
    original_sizes.iter().take(index).sum()
}

fn extract_line(fragments: &[Fragment], index: &mut usize) -> Option<Vec<u8>> {
    let start = *index;
    let mut i = *index;
    let mut ok = false;
    while let Some(fragment) = fragments.get(i) {
        i += 1;
        if fragment.is_plain_text() && fragment.data().contains(&b'\n') {
            ok = true;
            break;
        }
        if fragment.escape_sequence() == Some(&EscapeSequence::Other)
            && matches!(fragment.data(), b"\x1b[?25h" | b"\x1b[?2026l")
        {
            ok = true;
            break;
        }
    }
    if ok {
        *index = i;
        Some(collect_plain_text(&fragments[start..*index]))
    } else {
        None
    }
}

fn collect_plain_text<'a>(fragments: impl IntoIterator<Item = &'a Fragment<'a>>) -> Vec<u8> {
    fragments
        .into_iter()
        .filter(|f| f.is_plain_text())
        .flat_map(|f| f.data())
        .cloned()
        .collect()
}

fn can_be_wrapped(line: &[u8], terminal_width: u16) -> bool {
    const MARKER: &[u8] = b"://";

    let Some(i) = line.windows(MARKER.len()).rposition(|w| w == MARKER) else {
        return false;
    };

    let i = i + MARKER.len();

    if line[i..]
        .iter()
        .any(|&b| !b.is_ascii_graphic() && b != b'\r' && b != b'\n')
    {
        return false;
    }

    usize::from(terminal_width).saturating_sub(RIGHT_MARGIN)
        <= std::str::from_utf8(&line[..i])
            .map(unicode_width::UnicodeWidthStr::width)
            .unwrap_or(i)
            + line[i..].len()
}

fn can_follow_wrapped(line: &[u8]) -> bool {
    if line.len() < LEFT_MARGIN + 1 || &line[..LEFT_MARGIN] != INDENT {
        return false;
    }

    // ordered list
    if let Some(i) = line[LEFT_MARGIN..]
        .iter()
        .position(|&b| !b.is_ascii_digit())
        && i > 0
    {
        let i = LEFT_MARGIN + i;
        if matches!(line.get(i..i + 2), Some(b". " | b".\r" | b".\n")) {
            return false;
        }
    }

    // unordered list
    if matches!(
        line.get(LEFT_MARGIN..LEFT_MARGIN + 2),
        Some(b"- " | b"-\r" | b"-\n"),
    ) {
        return false;
    }

    line[LEFT_MARGIN].is_ascii_graphic()
}

fn can_be_wrapped_again(line: &[u8], terminal_width: u16) -> bool {
    if line.len() < LEFT_MARGIN + 1 || &line[..LEFT_MARGIN] != INDENT {
        return false;
    }

    usize::from(terminal_width).saturating_sub(RIGHT_MARGIN) <= line.len()
        && line[LEFT_MARGIN..]
            .iter()
            .all(|&b| b.is_ascii_graphic() || b == b'\r' || b == b'\n')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tty_text::fragment::FragmentList;
    use indoc::indoc;

    #[test]
    fn normal_text_after_wrapped_url() {
        assert_eq!(
            make_adjusted_string(
                indoc! {b"
                    \x1b[38;5;231m\xE2\x8F\xBA\x1b[39m \x1b[38;5;153mURL: https://example.c
                      om/long/long/path/to/r
                      esource and some more
                      text.
                "},
                28,
            ),
            indoc! {"
                \x1b[38;5;231m⏺\x1b[39m \x1b[38;5;153mURL: https://example.com/long/long/path/to/resource and some more
                  text.
            "},
        );
    }

    #[test]
    fn unordered_list() {
        assert_eq!(
            make_adjusted_string(
                indoc! {b"
                    \xE2\x8F\xBA unordered list

                      - https://example.c
                      om/1
                      - https://example.c
                      om/2
                      -
                      https://example.com
                      - https://example.c
                      om/4
                "},
                25,
            ),
            indoc! {"
                ⏺ unordered list

                  - https://example.com/1
                  - https://example.com/2
                  -
                  https://example.com
                  - https://example.com/4
            "}
        );
    }

    fn make_adjusted_string(data: &[u8], terminal_width: u16) -> String {
        let mut fragments = FragmentList::parse(data, false).into_inner();
        adjust_line_wrapping(&mut fragments, false, terminal_width);
        String::from_utf8(
            fragments
                .into_iter()
                .map(|f| f.data())
                .collect::<Vec<_>>()
                .concat(),
        )
        .unwrap()
    }
}
