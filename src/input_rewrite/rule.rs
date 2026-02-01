use super::escape::{EscapeError, parse_escaped_str};
use std::fmt;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RewriteRule {
    from: Vec<u8>, // non-empty
    to: Vec<u8>,
}

impl RewriteRule {
    /// Parse a string in the "FROM:TO" form and create a [`RewriteRule`].
    pub fn parse(s: &str) -> Result<Self, ParseError> {
        // Literal ':' in FROM requires \x3a escape
        let (from_str, to_str) = s.split_once(':').ok_or(ParseError::MissingColon)?;

        let from = parse_escaped_str(from_str)?;
        let to = parse_escaped_str(to_str)?;

        if from.is_empty() {
            return Err(ParseError::EmptyFrom);
        }

        Ok(RewriteRule { from, to })
    }

    pub fn from(&self) -> &[u8] {
        &self.from
    }

    pub fn to(&self) -> &[u8] {
        &self.to
    }
}

#[derive(Debug)]
pub enum ParseError {
    MissingColon,
    EmptyFrom,
    EscapeError(EscapeError),
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseError::MissingColon => write!(f, "missing ':' separator in rewrite rule"),
            ParseError::EmptyFrom => write!(f, "FROM pattern cannot be empty"),
            ParseError::EscapeError(e) => write!(f, "escape error: {}", e),
        }
    }
}

impl std::error::Error for ParseError {}

impl From<EscapeError> for ParseError {
    fn from(e: EscapeError) -> Self {
        ParseError::EscapeError(e)
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use crate::input_rewrite::escape::tests::{
        arb_invalid_escaped_string, arb_valid_escaped_string,
    };
    use proptest::prelude::*;
    use proptest::property_test;

    #[property_test]
    fn parse_correctness(#[strategy = arb_rule()] (expected, valid): (RewriteRule, String)) {
        prop_assert_eq!(RewriteRule::parse(&valid).unwrap(), expected);
    }

    #[property_test]
    fn parse_rejects_invalid(#[strategy = arb_invalid_rule_string()] invalid: String) {
        prop_assert!(RewriteRule::parse(&invalid).is_err());
    }

    impl RewriteRule {
        pub fn new_unchecked(from: Vec<u8>, to: Vec<u8>) -> Self {
            debug_assert!(!from.is_empty(), "FROM must be non-empty");
            RewriteRule { from, to }
        }
    }

    pub fn arb_rule() -> impl Strategy<Value = (RewriteRule, String)> {
        (arb_valid_from_part(), arb_valid_to_part()).prop_map(
            |((from, escaped_from), (to, escaped_to))| {
                (
                    RewriteRule { from, to },
                    format!("{escaped_from}:{escaped_to}"),
                )
            },
        )
    }

    fn arb_invalid_rule_string() -> impl Strategy<Value = String> {
        prop_oneof![
            arb_valid_to_part().prop_map(|(_, valid_to)| format!(":{valid_to}")),
            (arb_invalid_escaped_string(), arb_valid_to_part())
                .prop_map(|(invalid_from, (_, valid_to))| format!("{invalid_from}:{valid_to}")),
            (arb_valid_from_part(), arb_invalid_escaped_string())
                .prop_map(|((_, valid_from), invalid_to)| format!("{valid_from}:{invalid_to}")),
            (arb_valid_from_part(), arb_valid_to_part()).prop_filter_map(
                "colon must not appear in TO part for missing colon test",
                |((_, valid_from), (_, valid_to))| (!valid_to.contains(":"))
                    .then_some(format!("{valid_from}{valid_to}")),
            ),
        ]
    }

    fn arb_valid_from_part() -> impl Strategy<Value = (Vec<u8>, String)> {
        arb_valid_escaped_string()
            .prop_map(|(escaped_from, from)| (from, escaped_from))
            .prop_filter("FROM part must be non-empty", |(_, escaped_from)| {
                !escaped_from.is_empty()
            })
            .prop_filter("colon must not appear in FROM part", |(_, escaped_from)| {
                !escaped_from.contains(":")
            })
    }

    fn arb_valid_to_part() -> impl Strategy<Value = (Vec<u8>, String)> {
        arb_valid_escaped_string().prop_map(|(escaped_to, to)| (to, escaped_to))
    }
}
