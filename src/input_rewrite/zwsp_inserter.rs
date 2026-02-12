use std::io::{self, Write};

/// ZWSP (U+200B) UTF-8 encoding
const ZWSP: [u8; 3] = [0xe2, 0x80, 0x8b];

/// Wraps a `Write` and inserts a ZWSP before the first digit byte after an
/// Up/Down arrow CSI sequence.  All other bytes pass through unchanged.
///
/// This prevents Claude Code's AskUserQuestion from interpreting a leading
/// digit as an option-selection when the user has just navigated with arrow
/// keys to the free-text input field.
pub struct ZwspInserter<W> {
    inner: W,
    state: State,
}

/// CSI-aware state machine tracking whether the most recent complete sequence
/// was an Up or Down arrow.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Idle,
    EscSeen,
    CsiStart,
    CsiParam,
    Triggered,
}

impl<W: Write> ZwspInserter<W> {
    pub fn new(inner: W) -> Self {
        ZwspInserter {
            inner,
            state: State::Idle,
        }
    }
}

impl<W: Write> Write for ZwspInserter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        for &b in buf {
            match self.state {
                State::Idle => {
                    if b == 0x1b {
                        self.state = State::EscSeen;
                    }
                    self.inner.write_all(&[b])?;
                }
                State::EscSeen => {
                    if b == b'[' {
                        self.state = State::CsiStart;
                    } else if b == 0x1b {
                        // Another ESC restarts the sequence (e.g., \x1b\x1b[A (Alt+Up))
                    } else {
                        self.state = State::Idle;
                    }
                    self.inner.write_all(&[b])?;
                }
                State::CsiStart => {
                    if b == b'A' || b == b'B' {
                        self.state = State::Triggered;
                    } else if (0x20..=0x3f).contains(&b) {
                        self.state = State::CsiParam;
                    } else {
                        // Any other final byte (0x40-0x7e) ends the sequence
                        self.state = State::Idle;
                    }
                    self.inner.write_all(&[b])?;
                }
                State::CsiParam => {
                    if b == b'A' || b == b'B' {
                        self.state = State::Triggered;
                    } else if (0x20..=0x3f).contains(&b) {
                        // Stay in CsiParam
                    } else {
                        // Other final byte
                        self.state = State::Idle;
                    }
                    self.inner.write_all(&[b])?;
                }
                State::Triggered => {
                    if b.is_ascii_digit() {
                        self.inner.write_all(&ZWSP)?;
                        self.state = State::Idle;
                    } else if b == 0x1b {
                        self.state = State::EscSeen;
                    } else {
                        self.state = State::Idle;
                    }
                    self.inner.write_all(&[b])?;
                }
            }
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use proptest::property_test;
    use regex::bytes::Regex;

    #[property_test]
    fn removing_zwsp_recovers_input(#[strategy = arb_input()] input: Vec<u8>) {
        prop_assume!(!input.windows(3).any(|w| w == ZWSP));
        prop_assert_eq!(strip_zwsp(&run(&input)), input);
    }

    #[property_test]
    fn zwsp_is_always_followed_by_digit(#[strategy = arb_input()] input: Vec<u8>) {
        let violation = Regex::new(r"(?-u)\xe2\x80\x8b(?:[^0-9]|\z)").unwrap();
        prop_assert!(!violation.is_match(&run(&input)));
    }

    #[property_test]
    fn no_updown_arrow_means_passthrough(#[strategy = arb_no_updown_input()] input: Vec<u8>) {
        prop_assert_eq!(run(&input), input);
    }

    #[property_test]
    fn no_digit_means_passthrough(#[strategy = arb_no_digit_input()] input: Vec<u8>) {
        prop_assert_eq!(run(&input), input);
    }

    #[property_test]
    fn matches_reference_implementation(#[strategy = arb_input()] input: Vec<u8>) {
        prop_assert_eq!(run(&input), run_with_regex(&input));
    }

    #[property_test]
    fn chunk_split_invariant(
        #[strategy = arb_input()] first: Vec<u8>,
        #[strategy = arb_input()] second: Vec<u8>,
    ) {
        prop_assert_eq!(run_all(&[&first, &second]), run(&[first, second].concat()));
    }

    #[test]
    fn up_arrow_then_digit() {
        assert_eq!(run(b"\x1b[A5"), b"\x1b[A\xe2\x80\x8b5");
    }

    #[test]
    fn down_arrow_then_digit() {
        assert_eq!(run(b"\x1b[B0"), b"\x1b[B\xe2\x80\x8b0");
    }

    #[test]
    fn right_arrow_does_not_trigger() {
        assert_eq!(run(b"\x1b[C5"), b"\x1b[C5");
    }

    #[test]
    fn modified_up_arrow_then_digit() {
        assert_eq!(run(b"\x1b[1;2A3"), b"\x1b[1;2A\xe2\x80\x8b3");
    }

    #[test]
    fn arrow_then_non_digit_passes_through() {
        assert_eq!(run(b"\x1b[Aa"), b"\x1b[Aa");
    }

    #[test]
    fn consecutive_arrows() {
        assert_eq!(run(b"\x1b[A\x1b[B3"), b"\x1b[A\x1b[B\xe2\x80\x8b3");
    }

    #[test]
    fn only_first_digit_gets_zwsp() {
        assert_eq!(run(b"\x1b[A12"), b"\x1b[A\xe2\x80\x8b12");
    }

    #[test]
    fn digits_without_arrow() {
        assert_eq!(run(b"12345"), b"12345");
    }

    fn run(input: &[u8]) -> Vec<u8> {
        run_all(&[input])
    }

    fn run_all(inputs: &[&[u8]]) -> Vec<u8> {
        let mut inserter = ZwspInserter::new(Vec::new());
        for input in inputs {
            inserter.write_all(input).unwrap();
        }
        inserter.inner
    }

    /// Regex-based reference implementation: capture CSI Up/Down + following digit,
    /// replace with ZWSP inserted between them.
    fn run_with_regex(input: &[u8]) -> Vec<u8> {
        Regex::new(r"(\x1b\[[\x20-\x3f]*[AB])([0-9])")
            .unwrap()
            .replace_all(input, b"$1\xe2\x80\x8b$2".as_slice())
            .into_owned()
    }

    fn strip_zwsp(data: &[u8]) -> Vec<u8> {
        Regex::new(r"(?-u:\xe2\x80\x8b)")
            .unwrap()
            .replace_all(data, b"".as_slice())
            .into_owned()
    }

    fn arb_input() -> impl Strategy<Value = Vec<u8>> {
        concat_bytes(prop_oneof![
            2 => arb_target_csi(),
            2 => arb_non_target_csi(),
            2 => arb_digit(),
            1 => Just(vec![0x1b]),
            1 => any::<u8>().prop_map(|b| vec![b]),
        ])
    }

    fn arb_no_updown_input() -> impl Strategy<Value = Vec<u8>> {
        concat_bytes(prop_oneof![
            2 => arb_non_target_csi(),
            2 => arb_digit(),
            1 => Just(vec![0x1b]),
            1 => any::<u8>().prop_map(|b| vec![b]),
        ])
    }

    fn arb_no_digit_input() -> impl Strategy<Value = Vec<u8>> {
        concat_bytes(prop_oneof![
            2 => arb_target_csi(),
            2 => arb_non_target_csi(),
            2 => Just(vec![0x1b]),
            1 => arb_non_digit(),
        ])
    }

    fn concat_bytes(bytes: impl Strategy<Value = Vec<u8>>) -> impl Strategy<Value = Vec<u8>> {
        prop::collection::vec(bytes, 0..32)
            .prop_map(|bytes_vec| bytes_vec.into_iter().flatten().collect())
    }

    /// CSI Up/Down (A/B): sequences that trigger ZWSP insertion.
    /// Parameters may include private-use bytes (`:<=>?`), so the
    /// generated sequences are not always well-formed ECMA-48.
    fn arb_target_csi() -> impl Strategy<Value = Vec<u8>> {
        arb_csi(arb_csi_params(), prop::sample::select(&b"AB"[..]))
    }

    /// CSI C/D/H/J: sequences that do not trigger ZWSP insertion.
    /// Same caveat about parameters as [`arb_target_csi`].
    fn arb_non_target_csi() -> impl Strategy<Value = Vec<u8>> {
        arb_csi(arb_csi_params(), prop::sample::select(&b"CDHJ"[..]))
    }

    fn arb_csi(
        params: impl Strategy<Value = Vec<u8>> + 'static,
        final_byte: impl Strategy<Value = u8> + 'static,
    ) -> impl Strategy<Value = Vec<u8>> {
        (params, final_byte).prop_map(|(p, f)| [&b"\x1b["[..], &p, &[f]].concat())
    }

    fn arb_csi_params() -> impl Strategy<Value = Vec<u8>> {
        prop::collection::vec(0x30..=0x3fu8, 0..=3)
    }

    /// A single ASCII digit byte, which triggers ZWSP insertion after a target CSI.
    fn arb_digit() -> impl Strategy<Value = Vec<u8>> {
        (0x30..=0x39u8).prop_map(|b| vec![b])
    }

    /// A single non-digit byte.
    fn arb_non_digit() -> impl Strategy<Value = Vec<u8>> {
        prop_oneof![0x00u8..=0x2f, 0x3au8..=0xff].prop_map(|b| vec![b])
    }
}
