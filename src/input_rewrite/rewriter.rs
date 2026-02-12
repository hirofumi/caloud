use super::rule::RewriteRule;
use nix::errno::Errno;
use nix::poll::{PollFd, PollFlags};
use std::collections::HashSet;
use std::io::{self, Write};
use std::os::fd::AsFd;
use std::time::Duration;

/// Default timeout for flushing pending prefix bytes (ESC ambiguity resolution)
const DEFAULT_PENDING_TIMEOUT: Duration = Duration::from_millis(50);

pub struct InputRewriter {
    rules: Vec<RewriteRule>,
    buffer: Vec<u8>,
    pending_timeout: Duration,
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
            pending_timeout: DEFAULT_PENDING_TIMEOUT,
        }
    }

    /// Read from `fd` and write rewritten output to `writer`, using poll(2)
    /// to resolve prefix ambiguity via timeout.
    ///
    /// The underlying file descriptor must not also be read through a buffered
    /// reader (e.g. `std::io::Stdin`), because its internal buffer would hide
    /// data from poll(2).
    pub fn rewrite<W: Write>(&mut self, fd: impl AsFd, writer: &mut W) -> io::Result<()> {
        let fd = fd.as_fd();
        let mut pfd = PollFd::new(fd, PollFlags::POLLIN);
        let mut buf = [0u8; 4096];

        loop {
            if self.has_pending() {
                let timeout = self.pending_timeout.as_millis().min(u16::MAX as u128) as u16;
                match nix::poll::poll(std::slice::from_mut(&mut pfd), timeout) {
                    Ok(0) => {
                        // Timeout: flush pending bytes without waiting for longer matches
                        self.drain(writer, true)?;
                        writer.flush()?;
                        continue;
                    }
                    Ok(_) => {}
                    Err(Errno::EINTR) => continue,
                    Err(e) => return Err(e.into()),
                }
            }

            match nix::unistd::read(fd, &mut buf) {
                Ok(0) => {
                    self.drain(writer, true)?;
                    writer.flush()?;
                    return Ok(());
                }
                Ok(n) => {
                    self.push(&buf[..n]);
                    self.drain(writer, false)?;
                    writer.flush()?;
                }
                Err(Errno::EINTR) => continue,
                Err(e) => return Err(e.into()),
            }
        }
    }

    #[cfg(test)]
    fn with_pending_timeout(mut self, timeout: Duration) -> Self {
        self.pending_timeout = timeout;
        self
    }

    fn push(&mut self, data: &[u8]) {
        self.buffer.extend_from_slice(data);
    }

    fn has_pending(&self) -> bool {
        !self.buffer.is_empty()
    }

    /// Process the buffer and write output. When `force` is true,
    /// skip waiting for potentially longer matches (used on timeout/EOF).
    fn drain<W: Write>(&mut self, writer: &mut W, force: bool) -> io::Result<()> {
        let mut i = 0;
        let mut passthrough_from = 0;

        while i < self.buffer.len() {
            let remaining = &self.buffer[i..];

            if !force {
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
                    if passthrough_from < i {
                        writer.write_all(&self.buffer[passthrough_from..i])?;
                    }
                    if !rule.to().is_empty() {
                        writer.write_all(rule.to())?;
                    }
                    i += rule.from().len();
                    passthrough_from = i;
                    matched = true;
                    break;
                }
            }

            if !matched {
                i += 1;
            }
        }

        if passthrough_from < i {
            writer.write_all(&self.buffer[passthrough_from..i])?;
        }

        self.buffer.drain(..i);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input_rewrite::rule::RewriteRule;
    use proptest::prelude::*;
    use proptest::property_test;
    use std::collections::HashSet;
    use std::os::fd::{AsFd, OwnedFd};
    use std::thread;
    use std::time::Duration;

    #[property_test]
    fn rewrite_output_equals_naive_output(
        #[strategy = arb_alphabet_01_rules()] rules: Vec<RewriteRule>,
        #[strategy = arb_alphabet_01_bytes(0..64)] input: Vec<u8>,
    ) {
        let expected = rewrite_bytes_naively(&rules, &input);
        let output = rewrite_bytes(&mut InputRewriter::new(rules), &input);
        prop_assert_eq!(&output, &expected);
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

    #[test]
    fn short_timeout_flushes_prefix_before_continuation_arrives() {
        let mut rewriter = InputRewriter::new(vec![RewriteRule::parse(r"\x1bb:\x02").unwrap()])
            .with_pending_timeout(Duration::from_nanos(1));
        let (read_fd, write_fd) = nix::unistd::pipe().unwrap();

        let writer = thread::spawn(move || {
            write_all_fd(&write_fd, b"\x1b");
            thread::sleep(Duration::from_millis(10));
            write_all_fd(&write_fd, b"b");
        });

        let mut output = Vec::new();
        rewriter.rewrite(read_fd.as_fd(), &mut output).unwrap();
        writer.join().unwrap();

        // Timeout ≈ 0ms: "\x1b" flushed before "b" arrives — no match
        assert_eq!(output, b"\x1bb");
    }

    #[test]
    fn long_timeout_allows_split_bytes_to_combine() {
        let mut rewriter = InputRewriter::new(vec![RewriteRule::parse(r"\x1bb:\x02").unwrap()])
            .with_pending_timeout(Duration::from_secs(60));
        let (read_fd, write_fd) = nix::unistd::pipe().unwrap();

        let writer = thread::spawn(move || {
            write_all_fd(&write_fd, b"\x1b");
            thread::sleep(Duration::from_millis(10));
            write_all_fd(&write_fd, b"b");
        });

        let mut output = Vec::new();
        rewriter.rewrite(read_fd.as_fd(), &mut output).unwrap();
        writer.join().unwrap();

        // Timeout = 60s: "b" arrives well within timeout — match
        assert_eq!(output, b"\x02");
    }

    fn rewrite_bytes(rewriter: &mut InputRewriter, input: &[u8]) -> Vec<u8> {
        let (read_fd, write_fd) = nix::unistd::pipe().unwrap();
        write_all_fd(&write_fd, input);
        drop(write_fd);

        let mut output = Vec::new();
        rewriter.rewrite(read_fd.as_fd(), &mut output).unwrap();
        output
    }

    fn write_all_fd(fd: &OwnedFd, data: &[u8]) {
        let mut offset = 0;
        while offset < data.len() {
            let n = nix::unistd::write(fd, &data[offset..]).unwrap();
            assert!(n > 0);
            offset += n;
        }
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

    fn arb_alphabet_01_bytes(
        len: impl Into<prop::collection::SizeRange>,
    ) -> impl Strategy<Value = Vec<u8>> {
        prop::collection::vec(0u8..=1, len)
    }
}
