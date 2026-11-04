//! Compact binary wire format.
//!
//! The protocol is little-endian and self-describing only through explicit
//! tags. Fixed-width integers are copied directly; strings are length-prefixed
//! with a `u16` (symbols are short). The engine owns this codec instead of
//! depending on a general-purpose serialization framework, so the hot path has
//! no schema/reflection overhead and no unmaintained dependencies.
//!
//! # Format
//! - integers: little-endian
//! - string: `u16` byte length followed by UTF-8 bytes
//! - enum: one `u8` tag followed by variant fields
//!
//! Network transports frame payloads with a big-endian `u32` length prefix via
//! [`encode_frame`]; the receiver strips the prefix and calls [`decode`].
use std::sync::Arc;
use thiserror::Error;

/// Maximum accepted frame payload size (1 MiB).
///
/// Order and trade messages are well under 1 KiB; a tighter bound limits the
/// memory a misbehaving peer can pin per connection.
pub const MAX_FRAME_LEN: usize = 1024 * 1024;

/// Wire codec error.
#[derive(Debug, Error)]
pub enum WireError {
    #[error("unexpected end of input: needed {needed} bytes, {remaining} available")]
    UnexpectedEof { needed: usize, remaining: usize },

    #[error("string is too long for a u16 length prefix: {0} bytes")]
    StringTooLong(usize),

    #[error("invalid UTF-8 string: {0}")]
    InvalidUtf8(#[from] std::str::Utf8Error),

    #[error("invalid tag {tag} for {type_name}")]
    InvalidTag { type_name: &'static str, tag: u8 },

    #[error("{count} trailing bytes after message")]
    TrailingBytes { count: usize },

    #[error("frame length {len} exceeds maximum {max}")]
    FrameTooLarge { len: usize, max: usize },

    #[error("unknown symbol id {0}")]
    UnknownSymbol(u32),
}

/// Values that can be appended to a wire buffer.
pub trait WireEncode {
    /// Appends the encoding of `self` to `out`.
    fn encode(&self, out: &mut Vec<u8>) -> Result<(), WireError>;
}

/// Values that can be decoded from a [`WireReader`].
pub trait WireDecode: Sized {
    /// Decodes one value, advancing `reader`.
    fn decode(reader: &mut WireReader<'_>) -> Result<Self, WireError>;
}

/// A cursor over an encoded payload.
pub struct WireReader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> WireReader<'a> {
    /// Creates a reader over `buf`.
    #[must_use]
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    /// Returns the number of unread bytes.
    #[must_use]
    pub fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }

    /// Reads one byte.
    pub fn read_u8(&mut self) -> Result<u8, WireError> {
        Ok(self.read_exact(1)?[0])
    }

    /// Reads a little-endian `u32`.
    pub fn read_u32(&mut self) -> Result<u32, WireError> {
        let bytes = self.read_exact(4)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    /// Reads a little-endian `u64`.
    pub fn read_u64(&mut self) -> Result<u64, WireError> {
        let bytes = self.read_exact(8)?;
        Ok(u64::from_le_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]))
    }

    /// Reads a `u16`-length-prefixed UTF-8 string.
    pub fn read_str(&mut self) -> Result<&'a str, WireError> {
        let len = self.read_u16()? as usize;
        let bytes = self.read_exact(len)?;
        Ok(std::str::from_utf8(bytes)?)
    }

    /// Reads a `u16`-length-prefixed UTF-8 string into an `Arc<str>`.
    pub fn read_arc_str(&mut self) -> Result<Arc<str>, WireError> {
        let value = self.read_str()?;
        Ok(Arc::from(value))
    }

    /// Reads a `u16`-length-prefixed UTF-8 string into a `String`.
    pub fn read_string(&mut self) -> Result<String, WireError> {
        Ok(self.read_str()?.to_string())
    }

    /// Reads a little-endian `u16`.
    pub fn read_u16(&mut self) -> Result<u16, WireError> {
        let bytes = self.read_exact(2)?;
        Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
    }

    /// Ensures the whole input was consumed.
    pub fn finish(&self) -> Result<(), WireError> {
        if self.pos == self.buf.len() {
            Ok(())
        } else {
            Err(WireError::TrailingBytes {
                count: self.buf.len() - self.pos,
            })
        }
    }

    fn read_exact(&mut self, len: usize) -> Result<&'a [u8], WireError> {
        let remaining = self.remaining();
        if remaining < len {
            return Err(WireError::UnexpectedEof {
                needed: len,
                remaining,
            });
        }
        let start = self.pos;
        self.pos += len;
        Ok(&self.buf[start..self.pos])
    }
}

/// Extension methods for appending primitives to a `Vec<u8>`.
pub trait WireWrite {
    fn put_u8(&mut self, value: u8);
    fn put_u16(&mut self, value: u16);
    fn put_u32(&mut self, value: u32);
    fn put_u64(&mut self, value: u64);
    fn put_str(&mut self, value: &str) -> Result<(), WireError>;
}

impl WireWrite for Vec<u8> {
    #[inline]
    fn put_u8(&mut self, value: u8) {
        self.push(value);
    }

    #[inline]
    fn put_u16(&mut self, value: u16) {
        self.extend_from_slice(&value.to_le_bytes());
    }

    #[inline]
    fn put_u32(&mut self, value: u32) {
        self.extend_from_slice(&value.to_le_bytes());
    }

    #[inline]
    fn put_u64(&mut self, value: u64) {
        self.extend_from_slice(&value.to_le_bytes());
    }

    #[inline]
    fn put_str(&mut self, value: &str) -> Result<(), WireError> {
        let len = u16::try_from(value.len()).map_err(|_| WireError::StringTooLong(value.len()))?;
        self.put_u16(len);
        self.extend_from_slice(value.as_bytes());
        Ok(())
    }
}

/// Encodes `value` into a fresh buffer.
pub fn encode_to_vec<T: WireEncode>(value: &T) -> Result<Vec<u8>, WireError> {
    let mut out = Vec::with_capacity(64);
    value.encode(&mut out)?;
    Ok(out)
}

/// Decodes a complete payload, rejecting trailing bytes.
pub fn decode<T: WireDecode>(buf: &[u8]) -> Result<T, WireError> {
    let mut reader = WireReader::new(buf);
    let value = T::decode(&mut reader)?;
    reader.finish()?;
    Ok(value)
}

/// Encodes `value` with a big-endian `u32` length prefix.
pub fn encode_frame<T: WireEncode>(value: &T) -> Result<Vec<u8>, WireError> {
    let payload = encode_to_vec(value)?;
    if payload.len() > MAX_FRAME_LEN {
        return Err(WireError::FrameTooLarge {
            len: payload.len(),
            max: MAX_FRAME_LEN,
        });
    }

    let mut out = Vec::with_capacity(4 + payload.len());
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(&payload);
    Ok(out)
}

/// Decodes a complete frame (big-endian `u32` length prefix + payload).
pub fn decode_frame<T: WireDecode>(frame: &[u8]) -> Result<T, WireError> {
    if frame.len() < 4 {
        return Err(WireError::UnexpectedEof {
            needed: 4,
            remaining: frame.len(),
        });
    }
    let len = u32::from_be_bytes([frame[0], frame[1], frame[2], frame[3]]) as usize;
    if len > MAX_FRAME_LEN {
        return Err(WireError::FrameTooLarge {
            len,
            max: MAX_FRAME_LEN,
        });
    }
    if frame.len() != 4 + len {
        return Err(WireError::TrailingBytes {
            count: frame.len().saturating_sub(4 + len),
        });
    }
    decode(&frame[4..])
}

/// Decodes a payload whose `u32` length prefix was already consumed.
pub fn decode_frame_payload<T: WireDecode>(payload: &[u8]) -> Result<T, WireError> {
    decode(payload)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_primitive_roundtrip() {
        let mut buf = Vec::new();
        buf.put_u8(7);
        buf.put_u16(1_234);
        buf.put_u32(9_876_543);
        buf.put_u64(12_345_678_901);
        buf.put_str("BTCUSDT").unwrap();

        let mut reader = WireReader::new(&buf);
        assert_eq!(reader.read_u8().unwrap(), 7);
        assert_eq!(reader.read_u16().unwrap(), 1_234);
        assert_eq!(reader.read_u32().unwrap(), 9_876_543);
        assert_eq!(reader.read_u64().unwrap(), 12_345_678_901);
        assert_eq!(reader.read_str().unwrap(), "BTCUSDT");
        reader.finish().unwrap();
    }

    #[test]
    fn test_truncated_input() {
        let mut buf = Vec::new();
        buf.put_u64(1);

        let mut reader = WireReader::new(&buf[..3]);
        assert!(matches!(
            reader.read_u64(),
            Err(WireError::UnexpectedEof {
                needed: 8,
                remaining: 3
            })
        ));
    }

    #[test]
    fn test_trailing_bytes_rejected() {
        let mut buf = Vec::new();
        buf.put_u8(1);
        buf.put_u8(2);
        let mut reader = WireReader::new(&buf);
        assert_eq!(reader.read_u8().unwrap(), 1);
        assert!(matches!(
            reader.finish(),
            Err(WireError::TrailingBytes { count: 1 })
        ));
    }

    #[test]
    fn test_string_too_long() {
        let long = "x".repeat(usize::from(u16::MAX) + 1);
        let mut buf = Vec::new();
        assert!(matches!(
            buf.put_str(&long),
            Err(WireError::StringTooLong(_))
        ));
    }
}
