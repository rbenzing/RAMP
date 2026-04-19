use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

const DEFAULT_MAX_LINES: usize = 1000;

pub struct RingBuffer {
    buf: VecDeque<String>,
    max: usize,
}

impl RingBuffer {
    pub fn new(max: usize) -> Self {
        Self {
            buf: VecDeque::with_capacity(max),
            max,
        }
    }

    pub fn push(&mut self, line: String) {
        if self.buf.len() == self.max {
            self.buf.pop_front();
        }
        self.buf.push_back(line);
    }

    pub fn tail(&self, n: usize) -> Vec<&str> {
        let start = self.buf.len().saturating_sub(n);
        self.buf.range(start..).map(|s| s.as_str()).collect()
    }

    #[allow(dead_code)]
    pub fn all(&self) -> Vec<&str> {
        self.buf.iter().map(|s| s.as_str()).collect()
    }
}

/// Thread-safe shared log buffer.
#[derive(Clone)]
pub struct SharedLog(Arc<Mutex<RingBuffer>>);

impl SharedLog {
    pub fn new() -> Self {
        Self(Arc::new(Mutex::new(RingBuffer::new(DEFAULT_MAX_LINES))))
    }

    pub fn push(&self, line: String) {
        if let Ok(mut buf) = self.0.lock() {
            buf.push(line);
        }
    }

    pub fn tail(&self, n: usize) -> Vec<String> {
        if let Ok(buf) = self.0.lock() {
            buf.tail(n).into_iter().map(|s| s.to_owned()).collect()
        } else {
            vec![]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_buffer_evicts_oldest() {
        let mut buf = RingBuffer::new(3);
        buf.push("a".into());
        buf.push("b".into());
        buf.push("c".into());
        buf.push("d".into());
        let tail = buf.all();
        assert_eq!(tail, vec!["b", "c", "d"]);
    }

    #[test]
    fn tail_returns_last_n() {
        let mut buf = RingBuffer::new(100);
        for i in 0..10 {
            buf.push(format!("{i}"));
        }
        let t = buf.tail(3);
        assert_eq!(t, vec!["7", "8", "9"]);
    }
}
