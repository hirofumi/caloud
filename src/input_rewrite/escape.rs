use std::fmt;

/// Convert an escaped string into `Vec<u8>`.
///
/// Syntax:
/// - `\\`: backslash
/// - `\e`: ESC (0x1b)
/// - `\n`: LF (0x0a)
/// - `\r`: CR (0x0d)
/// - `\t`: TAB (0x09)
/// - `\xNN`: one byte encoded as two hexadecimal digits
pub fn parse_escaped_str(s: &str) -> Result<Vec<u8>, EscapeError> {
    let mut result = Vec::new();
    let mut chars = s.char_indices().peekable();

    while let Some((pos, ch)) = chars.next() {
        if ch == '\\' {
            match chars.next() {
                Some((_, '\\')) => result.push(b'\\'),
                Some((_, 'e')) => result.push(0x1b), // ESC
                Some((_, 'n')) => result.push(0x0a), // LF
                Some((_, 'r')) => result.push(0x0d), // CR
                Some((_, 't')) => result.push(0x09), // TAB
                Some((_, 'x')) => {
                    let hex_chars: String = chars.by_ref().take(2).map(|(_, c)| c).collect();
                    if hex_chars.len() != 2 {
                        return Err(EscapeError::InvalidHex { pos });
                    }
                    let byte = u8::from_str_radix(&hex_chars, 16)
                        .map_err(|_| EscapeError::InvalidHex { pos })?;
                    result.push(byte);
                }
                Some((escape_pos, escape_ch)) => {
                    return Err(EscapeError::UnknownEscape {
                        pos: escape_pos,
                        ch: escape_ch,
                    });
                }
                None => {
                    return Err(EscapeError::TrailingBackslash { pos });
                }
            }
        } else {
            let mut buffer = [0u8; 4];
            let bytes = ch.encode_utf8(&mut buffer);
            result.extend_from_slice(bytes.as_bytes());
        }
    }

    Ok(result)
}

#[derive(Debug)]
pub enum EscapeError {
    InvalidHex { pos: usize },
    UnknownEscape { pos: usize, ch: char },
    TrailingBackslash { pos: usize },
}

impl fmt::Display for EscapeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EscapeError::InvalidHex { pos } => {
                write!(f, "invalid hex escape sequence at position {}", pos)
            }
            EscapeError::UnknownEscape { pos, ch } => {
                write!(f, "unknown escape character '{}' at position {}", ch, pos)
            }
            EscapeError::TrailingBackslash { pos } => {
                write!(f, "trailing backslash at position {}", pos)
            }
        }
    }
}

impl std::error::Error for EscapeError {}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use proptest::prelude::*;
    use proptest::property_test;

    #[property_test]
    fn parse_inverts_escape(
        #[strategy = arb_valid_escaped_string()] (escaped, expected): (String, Vec<u8>),
    ) {
        prop_assert_eq!(parse_escaped_str(&escaped).unwrap(), expected);
    }

    #[property_test]
    fn parse_rejects_invalid_escape(#[strategy = arb_invalid_escaped_string()] invalid: String) {
        prop_assert!(parse_escaped_str(&invalid).is_err());
    }

    pub fn arb_valid_escaped_string() -> impl Strategy<Value = (String, Vec<u8>)> {
        prop::collection::vec(arb_valid_char(), 0..10).prop_map(|pairs| {
            let (escaped, expected): (String, Vec<Vec<u8>>) = pairs.into_iter().unzip();
            (escaped, expected.concat())
        })
    }

    pub fn arb_invalid_escaped_string() -> impl Strategy<Value = String> {
        let arb_valid = |n| {
            prop::collection::vec(arb_valid_char().prop_map(|(s, _)| s), 0..n)
                .prop_map(|s| s.concat())
        };
        prop_oneof![
            (arb_valid(5), arb_complete_invalid_escape(), arb_valid(5))
                .prop_map(|(valid1, invalid, valid2)| { format!("{valid1}{invalid}{valid2}") }),
            (arb_valid(9), arb_incomplete_escape())
                .prop_map(|(valid, incomplete)| { format!("{valid}{incomplete}") })
        ]
    }

    fn arb_valid_char() -> impl Strategy<Value = (String, Vec<u8>)> {
        prop_oneof![
            "[^\\\\]".prop_map(|c| (c.to_string(), c.into_bytes())),
            Just(r"\\").prop_map(|_| (r"\\".to_string(), vec![b'\\'])),
            Just(r"\e").prop_map(|_| (r"\e".to_string(), vec![0x1b])),
            Just(r"\n").prop_map(|_| (r"\n".to_string(), vec![0x0a])),
            Just(r"\r").prop_map(|_| (r"\r".to_string(), vec![0x0d])),
            Just(r"\t").prop_map(|_| (r"\t".to_string(), vec![0x09])),
            any::<u8>().prop_flat_map(|b| {
                prop_oneof![Just(format!(r"\x{b:02x}")), Just(format!(r"\x{b:02X}"))]
                    .prop_map(move |s| (s, vec![b]))
            }),
        ]
    }

    fn arb_complete_invalid_escape() -> impl Strategy<Value = String> {
        prop_oneof![
            "[^\\\\enrtx]".prop_map(|c| format!(r"\{c}")),
            "[^0-9a-fA-F]".prop_map(|h| format!(r"\x{h}")),
            ("[0-9a-fA-F]", "[^0-9a-fA-F]").prop_map(|(h, l)| format!(r"\x{h}{l}")),
        ]
    }

    fn arb_incomplete_escape() -> impl Strategy<Value = String> {
        prop_oneof![
            Just(r"\").prop_map(|s| s.to_string()),
            Just(r"\x").prop_map(|s| s.to_string()),
            "[0-9a-fA-F]".prop_map(|h| format!(r"\x{h}")),
        ]
    }
}
