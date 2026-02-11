use super::rule::RewriteRule;
use std::collections::HashSet;
use std::io::{self, Write};
use std::os::fd::RawFd;
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
    /// `fd` must not be wrapped in a buffered reader (e.g. `std::io::Stdin`),
    /// because its internal buffer would hide data from poll(2).
    pub fn rewrite<W: Write>(&mut self, fd: RawFd, writer: &mut W) -> io::Result<()> {
        let mut pfd = nix::libc::pollfd {
            fd,
            events: nix::libc::POLLIN,
            revents: 0,
        };
        let mut buf = [0u8; 4096];

        loop {
            let timeout = if self.has_pending() {
                self.pending_timeout.as_millis().min(i32::MAX as u128) as i32
            } else {
                -1
            };

            pfd.revents = 0;
            let poll_ret = unsafe { nix::libc::poll(&mut pfd, 1, timeout) };

            if poll_ret < 0 {
                let err = io::Error::last_os_error();
                if err.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(err);
            }

            if poll_ret == 0 {
                // Timeout: flush pending bytes without waiting for longer matches
                self.drain(writer, true)?;
                writer.flush()?;
                continue;
            }

            let n = unsafe { nix::libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };

            if n < 0 {
                let err = io::Error::last_os_error();
                if err.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(err);
            }

            if n == 0 {
                self.drain(writer, true)?;
                writer.flush()?;
                return Ok(());
            }

            self.push(&buf[..n as usize]);
            self.drain(writer, false)?;
            writer.flush()?;
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
    fn drain<W: Write>(&mut self, writer: &mut W, force: bool) -> io::Result<u64> {
        let mut written = 0u64;
        let mut i = 0;

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
        let (read_fd, write_fd) = pipe();

        let writer = thread::spawn(move || {
            write_all(write_fd, b"\x1b");
            thread::sleep(Duration::from_millis(10));
            write_all(write_fd, b"b");
            close(write_fd);
        });

        let mut output = Vec::new();
        rewriter.rewrite(read_fd, &mut output).unwrap();
        close(read_fd);
        writer.join().unwrap();

        // Timeout ≈ 0ms: "\x1b" flushed before "b" arrives — no match
        assert_eq!(output, b"\x1bb");
    }

    #[test]
    fn long_timeout_allows_split_bytes_to_combine() {
        let mut rewriter = InputRewriter::new(vec![RewriteRule::parse(r"\x1bb:\x02").unwrap()])
            .with_pending_timeout(Duration::from_secs(60));
        let (read_fd, write_fd) = pipe();

        let writer = thread::spawn(move || {
            write_all(write_fd, b"\x1b");
            thread::sleep(Duration::from_millis(10));
            write_all(write_fd, b"b");
            close(write_fd);
        });

        let mut output = Vec::new();
        rewriter.rewrite(read_fd, &mut output).unwrap();
        close(read_fd);
        writer.join().unwrap();

        // Timeout = 60s: "b" arrives well within timeout — match
        assert_eq!(output, b"\x02");
    }

    fn rewrite_bytes(rewriter: &mut InputRewriter, input: &[u8]) -> Vec<u8> {
        let (read_fd, write_fd) = pipe();
        write_all(write_fd, input);
        close(write_fd);

        let mut output = Vec::new();
        rewriter.rewrite(read_fd, &mut output).unwrap();
        close(read_fd);
        output
    }

    fn pipe() -> (RawFd, RawFd) {
        let mut fds = [0i32; 2];
        assert_eq!(unsafe { nix::libc::pipe(fds.as_mut_ptr()) }, 0);
        (fds[0], fds[1])
    }

    fn write_all(fd: RawFd, data: &[u8]) {
        let mut offset = 0;
        while offset < data.len() {
            let n = unsafe {
                nix::libc::write(fd, data[offset..].as_ptr().cast(), data.len() - offset)
            };
            assert!(n > 0);
            offset += n as usize;
        }
    }

    fn close(fd: RawFd) {
        assert_eq!(unsafe { nix::libc::close(fd) }, 0);
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
