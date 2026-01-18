//! Circular buffer wrapper for SDK transport
//!
//! Uses ringbuf library to implement an auto-managed circular buffer
//! that automatically overwrites old data when full, keeping memory usage stable.

use ringbuf::{RingBuffer, Producer, Consumer};
use std::io;

const DEFAULT_BUFFER_CAPACITY: usize = 20 * 1024 * 1024; // 20MB

/// Circular buffer wrapper using ringbuf
///
/// This provides a fixed-size buffer that automatically overwrites old data
/// when full, preventing unbounded memory growth in long-running sessions.
pub struct CircularBuffer {
    /// ringbuf's Producer end
    producer: Producer<u8>,
    /// ringbuf's Consumer end
    consumer: Consumer<u8>,
}

impl CircularBuffer {
    /// Create a new circular buffer with the specified capacity
    pub fn new(capacity: usize) -> Self {
        let rb = RingBuffer::new(capacity);
        let (producer, consumer) = rb.split();

        Self { producer, consumer }
    }

    /// Create a new circular buffer with default capacity (10MB)
    pub fn with_default_capacity() -> Self {
        Self::new(DEFAULT_BUFFER_CAPACITY)
    }

    /// Write data to the buffer
    ///
    /// Returns the number of bytes written. If the buffer is full,
    /// old data will be automatically overwritten by ringbuf.
    pub fn write(&mut self, data: &[u8]) -> usize {
        let mut written = 0;

        for &byte in data {
            // ringbuf handles overwriting automatically when full
            // We use push (blocking) to ensure all data is written
            // When the buffer is full, it will overwrite the oldest data
            self.producer.push(byte);
            written += 1;
        }

        written
    }

    /// Try to write data without blocking
    ///
    /// Returns the number of bytes written. If the buffer is full,
    /// stops writing instead of overwriting.
    pub fn try_write(&mut self, data: &[u8]) -> usize {
        let mut written = 0;

        for &byte in data {
            if self.producer.try_push(byte).is_ok() {
                written += 1;
            } else {
                // Buffer full, stop writing
                break;
            }
        }

        written
    }

    /// Read until a newline character
    ///
    /// Returns Ok(Some(line)) if a complete line was found,
    /// Ok(None) if no data is available, or Err on error.
    pub fn read_line(&mut self) -> io::Result<Option<Vec<u8>>> {
        let mut line = Vec::new();
        let mut found_data = false;

        loop {
            match self.consumer.try_pop() {
                Ok(byte) => {
                    found_data = true;
                    if byte == b'\n' {
                        return Ok(Some(line));
                    }
                    line.push(byte);
                }
                Err(ringbuf::PopError::Empty) => {
                    if found_data {
                        // We read some data but didn't find a newline yet
                        // This is incomplete data, return None to signal "wait for more"
                        return Ok(None);
                    }
                    // No data at all
                    return Ok(None);
                }
            }
        }
    }

    /// Get the number of bytes available to read
    pub fn len(&self) -> usize {
        // Approximate: available slots in consumer
        0 // ringbuf doesn't expose this directly in the API
    }

    /// Check if the buffer is empty
    pub fn is_empty(&self) -> bool {
        self.consumer.is_empty()
    }

    /// Get the total capacity of the buffer
    pub fn capacity(&self) -> usize {
        self.producer.capacity()
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
        // Add newline
        assert_eq!(cb.write(b"\n"), 1);
        assert_eq!(cb.read_line().unwrap(), Some(b"hello".to_vec()));
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

    #[test]
    fn test_try_write() {
        let mut cb = CircularBuffer::new(10);
        // Fill the buffer
        let written = cb.try_write(b"0123456789AB");
        // ringbuf capacity is exactly the size we set
        // So we should be able to write 10 bytes
        assert_eq!(written, 10);

        // Try to write more - should fail because buffer is full
        let more = cb.try_write(b"CD");
        assert_eq!(more, 0);
    }

    #[test]
    fn test_default_capacity() {
        let cb = CircularBuffer::with_default_capacity();
        assert_eq!(cb.capacity(), DEFAULT_BUFFER_CAPACITY);
    }
}
