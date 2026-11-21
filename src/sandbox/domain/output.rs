//! Keeps only the last bytes of a command's output, so memory stays bounded however much it prints.

#[derive(Debug)]
pub struct OutputTail {
    bytes: Vec<u8>,
    max_bytes: usize,
    truncated: bool,
}

impl OutputTail {
    pub fn new(max_bytes: usize) -> Self {
        Self { bytes: Vec::new(), max_bytes: max_bytes.max(1), truncated: false }
    }

    pub fn push(&mut self, chunk: &[u8]) {
        self.bytes.extend_from_slice(chunk);
        if self.bytes.len() > self.max_bytes {
            let excess = self.bytes.len() - self.max_bytes;
            self.bytes.drain(..excess);
            self.truncated = true;
        }
    }

    pub fn is_truncated(&self) -> bool {
        self.truncated
    }

    /// The kept text (lossy UTF-8). After truncation the first, partial line is dropped.
    pub fn render(&self) -> String {
        let text = String::from_utf8_lossy(&self.bytes);
        if !self.truncated {
            return text.into_owned();
        }
        text.split_once('\n').map_or_else(String::new, |(_, rest)| rest.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_output_is_kept_verbatim() {
        let mut tail = OutputTail::new(64);
        tail.push(b"a\n");
        tail.push(b"b\n");
        assert!(!tail.is_truncated());
        assert_eq!(tail.render(), "a\nb\n");
    }

    #[test]
    fn keeps_the_last_bytes_and_drops_the_cut_line() {
        let mut tail = OutputTail::new(16);
        tail.push(&[b'x'; 40]);
        tail.push(b"\nlast line\n");
        assert!(tail.is_truncated());
        assert_eq!(tail.render(), "last line\n");
        assert!(tail.bytes.len() <= 16);
    }

    #[test]
    fn a_single_cut_line_renders_empty() {
        let mut tail = OutputTail::new(8);
        tail.push(&[b'x'; 100]);
        assert_eq!(tail.render(), "");
        assert_eq!(OutputTail::new(0).max_bytes, 1);
    }
}
