use crate::tty_text::fragment::{Fragment, FragmentList};
use crate::tty_text::reformat::Reformatter;

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

    pub fn read_fragments(&mut self, formatter: &Reformatter) -> Vec<Fragment<'_>> {
        let fragments = FragmentList::parse(&self.data[self.start..self.end], self.is_full());
        let (consumed, fragments) = formatter.reformat(fragments, self.is_full());
        self.start += consumed;
        fragments
    }

    pub fn extend_from_read(&mut self, mut r: impl std::io::Read) -> std::io::Result<usize> {
        if 0 < self.start && N <= 2 * self.end {
            self.data.copy_within(self.start..self.end, 0);
            self.end -= self.start;
            self.start = 0;
        }
        let n = r.read(&mut self.data[self.end..])?;
        self.end += n;
        Ok(n)
    }
}
