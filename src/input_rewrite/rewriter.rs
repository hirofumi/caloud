use super::rule::RewriteRule;
use std::collections::HashSet;
use std::io::{self, Read, Write};

pub struct InputRewriter {
    rules: Vec<RewriteRule>,
    buffer: Vec<u8>,
}

impl InputRewriter {
    pub fn new(rules: Vec<RewriteRule>) -> Self {
        let mut rules: Vec<RewriteRule> = {
            let mut seen: HashSet<Vec<u8>> = HashSet::with_capacity(rules.len());
            rules
                .into_iter()
                .filter(|rule| seen.insert(rule.from().to_vec())) // dedup (first wins)
                .collect::<Vec<_>>()
        };

        rules.sort_by_key(|rule| usize::MAX - rule.from().len()); // for longest-match behavior

        InputRewriter {
            rules,
            buffer: Vec::new(),
        }
    }

    pub fn rewrite<R: Read, W: Write>(
        &mut self,
        reader: &mut R,
        writer: &mut W,
    ) -> io::Result<u64> {
        if self.rules.is_empty() {
            return io::copy(reader, writer);
        }

        let mut total_written = 0u64;
        let mut read_buffer = [0u8; 4096];

        loop {
            let n = reader.read(&mut read_buffer)?;
            if n == 0 {
                total_written += self.rewrite_and_flush(writer, true)?;
                break;
            }
            self.buffer.extend_from_slice(&read_buffer[..n]);
            total_written += self.rewrite_and_flush(writer, false)?;
        }

        Ok(total_written)
    }

    fn rewrite_and_flush<W: Write>(&mut self, writer: &mut W, at_eof: bool) -> io::Result<u64> {
        let mut written = 0u64;
        let mut i = 0;

        while i < self.buffer.len() {
            let remaining = &self.buffer[i..];

            if !at_eof {
                let might_match_longer_rule = self.rules.iter().any(|rule| {
                    rule.from().len() > remaining.len() && rule.from().starts_with(remaining)
                });
                if might_match_longer_rule {
                    break;
                }
            }

            let mut matched = false;
            for rule in &self.rules {
                if remaining.starts_with(rule.from()) {
                    if !rule.to().is_empty() {
                        writer.write_all(rule.to())?;
                        written += rule.to().len() as u64;
                    }
                    i += rule.from().len();
                    matched = true;
                    break;
                }
            }

            if !matched {
                writer.write_all(&remaining[..1])?;
                written += 1;
                i += 1;
            }
        }

        self.buffer.drain(..i);
        Ok(written)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input_rewrite::rule::RewriteRule;
    use proptest::prelude::*;
    use proptest::property_test;
    use std::collections::HashSet;

    #[property_test]
    fn rewrite_output_equals_naive_output(
        #[strategy = arb_alphabet_01_rules()] rules: Vec<RewriteRule>,
        #[strategy = arb_alphabet_01_chunks()] chunks: Vec<Vec<u8>>,
    ) {
        let expected = rewrite_bytes_naively(
            &rules,
            &chunks.iter().flatten().copied().collect::<Vec<_>>(),
        );
        let mut output = Vec::new();
        let n = InputRewriter::new(rules)
            .rewrite(&mut ChunkReader::new(chunks), &mut output)
            .unwrap();
        prop_assert_eq!(&output, &expected);
        prop_assert_eq!(n, output.len() as u64);
    }

    #[test]
    fn first_rule_wins() {
        let mut rewriter = InputRewriter::new(vec![
            RewriteRule::parse("a:first").unwrap(),
            RewriteRule::parse("a:second").unwrap(),
        ]);
        assert_eq!(rewrite_bytes(&mut rewriter, b"a"), b"first");
    }

    #[test]
    fn longest_match() {
        let mut rewriter = InputRewriter::new(vec![
            RewriteRule::parse(r"a:short").unwrap(),
            RewriteRule::parse(r"ab:long").unwrap(),
        ]);
        assert_eq!(rewrite_bytes(&mut rewriter, b"ab"), b"long");
    }

    #[test]
    fn longest_match_across_chunk_boundaries() {
        let mut rewriter = InputRewriter::new(vec![
            RewriteRule::parse(r"a:short").unwrap(),
            RewriteRule::parse(r"ab:long").unwrap(),
        ]);
        assert_eq!(rewrite_chunks(&mut rewriter, &[b"a", b"b"]), b"long");
    }

    #[test]
    fn eof_flushes_unmatched_buffer_as_raw() {
        let mut rewriter = InputRewriter::new(vec![RewriteRule::parse("abc:x").unwrap()]);
        assert_eq!(rewrite_bytes(&mut rewriter, b"ab"), b"ab");
    }

    #[test]
    fn output_is_not_reprocessed() {
        let mut rewriter = InputRewriter::new(vec![
            RewriteRule::parse("a:b").unwrap(),
            RewriteRule::parse("b:a").unwrap(),
        ]);
        assert_eq!(rewrite_bytes(&mut rewriter, b"ab"), b"ba");
    }

    fn rewrite_chunks(rewriter: &mut InputRewriter, chunks: &[&[u8]]) -> Vec<u8> {
        let mut output = Vec::new();
        rewriter
            .rewrite(
                &mut ChunkReader::new(chunks.iter().map(|chunk| chunk.to_vec()).collect()),
                &mut output,
            )
            .unwrap();
        output
    }

    fn rewrite_bytes(rewriter: &mut InputRewriter, input: &[u8]) -> Vec<u8> {
        let mut output = Vec::new();
        rewriter
            .rewrite(&mut io::Cursor::new(input), &mut output)
            .unwrap();
        output
    }

    /// Reference implementation: process input in one pass as a pure function.
    fn rewrite_bytes_naively(rules: &[RewriteRule], input: &[u8]) -> Vec<u8> {
        let rules = {
            let mut seen = HashSet::new();
            rules
                .iter()
                .filter(|r| seen.insert(r.from().to_vec())) // dedup (first wins)
                .collect::<Vec<_>>()
        };

        let mut output = Vec::new();

        let mut i = 0;
        while i < input.len() {
            let longest_match = rules
                .iter()
                .filter(|r| input[i..].starts_with(r.from()))
                .max_by_key(|r| r.from().len());
            match longest_match {
                Some(rule) => {
                    output.extend_from_slice(rule.to());
                    i += rule.from().len();
                }
                None => {
                    output.push(input[i]);
                    i += 1;
                }
            }
        }

        output
    }

    fn arb_alphabet_01_rules() -> impl Strategy<Value = Vec<RewriteRule>> {
        prop::collection::vec(
            (
                arb_alphabet_01_bytes(1..=6),
                prop::collection::vec(any::<u8>(), 0..=16),
            )
                .prop_map(|(from, to)| RewriteRule::new_unchecked(from, to)),
            0..8,
        )
    }

    fn arb_alphabet_01_chunks() -> impl Strategy<Value = Vec<Vec<u8>>> {
        prop::collection::vec(arb_alphabet_01_bytes(0..16), 0..8)
    }

    fn arb_alphabet_01_bytes(
        len: impl Into<prop::collection::SizeRange>,
    ) -> impl Strategy<Value = Vec<u8>> {
        prop::collection::vec(0u8..=1, len)
    }

    struct ChunkReader {
        chunks: Vec<Vec<u8>>,
        index: usize,
        offset: usize,
    }

    impl ChunkReader {
        fn new(chunks: Vec<Vec<u8>>) -> Self {
            ChunkReader {
                chunks,
                index: 0,
                offset: 0,
            }
        }
    }

    impl Read for ChunkReader {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            while self.index < self.chunks.len() {
                let chunk = &self.chunks[self.index];
                let remaining = &chunk[self.offset..];
                if remaining.is_empty() {
                    self.index += 1;
                    self.offset = 0;
                    continue;
                }
                let n = usize::min(remaining.len(), buf.len());
                buf[..n].copy_from_slice(&remaining[..n]);
                self.offset += n;
                if self.offset >= chunk.len() {
                    self.index += 1;
                    self.offset = 0;
                }
                return Ok(n);
            }
            Ok(0)
        }
    }
}
