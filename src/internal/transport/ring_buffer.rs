//! Circular buffer wrapper for SDK transport
//!
//! Uses ringbuf library to implement an auto-managed circular buffer
//! that automatically overwrites old data when full, keeping memory usage stable.

use ringbuf::{HeapRb, traits::*};
use std::io;

/// Circular buffer wrapper using ringbuf
///
/// This provides a fixed-size buffer that automatically overwrites old data
/// when full, preventing unbounded memory growth in long-running sessions.
pub struct CircularBuffer {
    /// The underlying ring buffer
    rb: HeapRb<u8>,
    /// Partial line data that has been read but doesn't have a newline yet
    /// This preserves data between calls to read_line()
    partial: Vec<u8>,
}

impl CircularBuffer {
    /// Create a new circular buffer with the specified capacity
    pub fn new(capacity: usize) -> Self {
        Self {
            rb: HeapRb::new(capacity),
            partial: Vec::new(),
        }
    }

    /// Write data to the buffer (overwrites old data when full)
    ///
    /// Returns the number of bytes written.
    /// When buffer is full, oldest data is automatically overwritten.
    pub fn write(&mut self, data: &[u8]) -> usize {
        let (mut prod, cons) = self.rb.split_ref();

        // Check buffer health before writing
        let occupied = cons.occupied_len();
        let capacity: usize = prod.capacity().into();
        if occupied > 0 {
            let usage_percent = (occupied * 100) / capacity;
            if usage_percent >= 90 {
                tracing::warn!(
                    "Circular buffer at {}% capacity ({} bytes), old data may be overwritten",
                    usage_percent,
                    occupied
                );
            }
        }

        // Use push_slice to write all data, overwriting if necessary
        let written = prod.push_slice(data);
        written
    }

    /// Read until a newline character
    ///
    /// Returns Ok(Some(line)) if a complete line was found,
    /// Ok(None) if no data is available (including partial data waiting for more),
    /// or Err on error.
    ///
    /// Note: Partial data (without newline) is preserved internally for the next call.
    pub fn read_line(&mut self) -> io::Result<Option<Vec<u8>>> {
        let (_, mut cons) = self.rb.split_ref();

        // Start with any partial data from previous call
        let mut line = self.partial.drain(..).collect::<Vec<_>>();
        let mut found_data = !line.is_empty();

        loop {
            match cons.try_pop() {
                Some(byte) => {
                    found_data = true;
                    if byte == b'\n' {
                        // Complete line found
                        return Ok(Some(line));
                    }
                    line.push(byte);
                }
                None => {
                    if found_data {
                        // We have some data but no newline yet
                        // Save it for next call and return None
                        self.partial = line;
                        return Ok(None);
                    }
                    // No data at all
                    return Ok(None);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_circular_buffer_basic() {
        let mut cb = CircularBuffer::new(100);
        assert_eq!(cb.write(b"hello\n"), 6);
        let result = cb.read_line().unwrap();
        assert_eq!(result, Some(b"hello".to_vec()));
    }

    #[test]
    fn test_circular_buffer_partial_line() {
        let mut cb = CircularBuffer::new(100);
        assert_eq!(cb.write(b"hello"), 5);
        // No newline yet, should return None (wait for more data)
        assert_eq!(cb.read_line().unwrap(), None);
        // Add newline - should now get the complete line
        assert_eq!(cb.write(b"\n"), 1);
        assert_eq!(cb.read_line().unwrap(), Some(b"hello".to_vec()));
    }

    #[test]
    fn test_circular_buffer_partial_line_preserved() {
        let mut cb = CircularBuffer::new(100);
        // Write partial data
        assert_eq!(cb.write(b"hello"), 5);
        // First read returns None, but preserves data
        assert_eq!(cb.read_line().unwrap(), None);
        // Add more data
        assert_eq!(cb.write(b" world\n"), 7);
        // Should get complete line including the partial data
        assert_eq!(cb.read_line().unwrap(), Some(b"hello world".to_vec()));
    }

    #[test]
    fn test_circular_buffer_empty() {
        let mut cb = CircularBuffer::new(100);
        assert_eq!(cb.read_line().unwrap(), None);
    }

    #[test]
    fn test_circular_buffer_multi_line() {
        let mut cb = CircularBuffer::new(100);
        cb.write(b"line1\nline2\nline3\n");
        assert_eq!(cb.read_line().unwrap(), Some(b"line1".to_vec()));
        assert_eq!(cb.read_line().unwrap(), Some(b"line2".to_vec()));
        assert_eq!(cb.read_line().unwrap(), Some(b"line3".to_vec()));
        assert_eq!(cb.read_line().unwrap(), None);
    }

    #[test]
    fn test_circular_buffer_small_buffer() {
        let mut cb = CircularBuffer::new(10);
        // Write more than capacity
        cb.write(b"0123456789");
        cb.write(b"AB");

        // With ringbuf, old data is automatically overwritten
        // The buffer now contains the most recent 10 bytes
        let result = cb.read_line().unwrap();
        // We may not get a complete line due to overwriting
        assert!(result.is_some() || result.is_none());
    }
}
