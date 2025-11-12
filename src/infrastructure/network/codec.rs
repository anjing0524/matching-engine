/// 编解码器实现

use crate::protocol::{NewOrderRequest, TradeNotification};
use serde::{Deserialize, Serialize};
use std::marker::PhantomData;

/// 编解码器trait
pub trait Codec: Send {
    type Item: Send;
    type Error: std::error::Error + Send;

    /// 解码（零拷贝）
    fn decode(&mut self, buf: &[u8]) -> Result<Option<Self::Item>, Self::Error>;

    /// 编码（零拷贝）
    fn encode(&mut self, item: &Self::Item, buf: &mut [u8]) -> Result<usize, Self::Error>;
}

/// Bincode编解码器
///
/// 使用bincode进行高效二进制序列化
pub struct BincodeCodec<T> {
    _phantom: PhantomData<T>,
}

impl<T> BincodeCodec<T> {
    pub fn new() -> Self {
        Self {
            _phantom: PhantomData,
        }
    }
}

impl<T> Default for BincodeCodec<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> Codec for BincodeCodec<T>
where
    T: Serialize + for<'de> Deserialize<'de> + Send,
{
    type Item = T;
    type Error = BincodeError;

    fn decode(&mut self, buf: &[u8]) -> Result<Option<Self::Item>, Self::Error> {
        if buf.is_empty() {
            return Ok(None);
        }

        // Decode using bincode v2 serde support
        let decoded: T = bincode::serde::decode_from_slice(buf, bincode::config::standard())
            .map(|(item, _size)| item)
            .map_err(|_| BincodeError(bincode::error::EncodeError::Other("Decode failed")))?;
        Ok(Some(decoded))
    }

    fn encode(&mut self, item: &Self::Item, buf: &mut [u8]) -> Result<usize, Self::Error> {
        // Encode to vec first, then copy to buffer
        let encoded = bincode::serde::encode_to_vec(item, bincode::config::standard())?;
        if encoded.len() > buf.len() {
            return Err(BincodeError(bincode::error::EncodeError::Other(
                "Buffer too small",
            )));
        }
        buf[..encoded.len()].copy_from_slice(&encoded);
        Ok(encoded.len())
    }
}

/// Bincode error wrapper
#[derive(Debug)]
pub struct BincodeError(bincode::error::EncodeError);

impl std::fmt::Display for BincodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Bincode error: {:?}", self.0)
    }
}

impl std::error::Error for BincodeError {}

impl From<bincode::error::EncodeError> for BincodeError {
    fn from(err: bincode::error::EncodeError) -> Self {
        Self(err)
    }
}

impl From<bincode::error::DecodeError> for BincodeError {
    fn from(_err: bincode::error::DecodeError) -> Self {
        // Convert decode error to encode error for simplicity
        Self(bincode::error::EncodeError::Other("Decode error"))
    }
}

/// 长度前缀编解码器
///
/// 在消息前添加4字节长度前缀
pub struct LengthDelimitedCodec<C> {
    inner: C,
    max_frame_len: usize,
}

impl<C> LengthDelimitedCodec<C> {
    pub fn new(inner: C) -> Self {
        Self {
            inner,
            max_frame_len: 1024 * 1024, // 1MB默认上限
        }
    }

    pub fn with_max_frame_len(inner: C, max_frame_len: usize) -> Self {
        Self {
            inner,
            max_frame_len,
        }
    }
}

impl<C> Codec for LengthDelimitedCodec<C>
where
    C: Codec,
{
    type Item = C::Item;
    type Error = CodecError<C::Error>;

    fn decode(&mut self, buf: &[u8]) -> Result<Option<Self::Item>, Self::Error> {
        if buf.len() < 4 {
            return Ok(None); // 需要更多数据
        }

        // 读取长度前缀
        let len_bytes = [buf[0], buf[1], buf[2], buf[3]];
        let len = u32::from_be_bytes(len_bytes) as usize;

        if len > self.max_frame_len {
            return Err(CodecError::FrameTooLarge {
                len,
                max: self.max_frame_len,
            });
        }

        if buf.len() < 4 + len {
            return Ok(None); // 需要更多数据
        }

        // 解码实际消息
        self.inner
            .decode(&buf[4..4 + len])
            .map_err(CodecError::Inner)
    }

    fn encode(&mut self, item: &Self::Item, buf: &mut [u8]) -> Result<usize, Self::Error> {
        if buf.len() < 4 {
            return Err(CodecError::BufferTooSmall);
        }

        // 编码消息到临时缓冲区（跳过前4字节）
        let size = self
            .inner
            .encode(item, &mut buf[4..])
            .map_err(CodecError::Inner)?;

        if size > self.max_frame_len {
            return Err(CodecError::FrameTooLarge {
                len: size,
                max: self.max_frame_len,
            });
        }

        // 写入长度前缀
        let len_bytes = (size as u32).to_be_bytes();
        buf[0..4].copy_from_slice(&len_bytes);

        Ok(4 + size)
    }
}

/// 编解码错误
#[derive(Debug, thiserror::Error)]
pub enum CodecError<E: std::error::Error> {
    #[error("Frame too large: {len} bytes (max: {max})")]
    FrameTooLarge { len: usize, max: usize },

    #[error("Buffer too small")]
    BufferTooSmall,

    #[error("Inner codec error: {0}")]
    Inner(E),
}

/// 订单协议编解码器
///
/// 专门用于交易订单的编解码
pub type OrderCodec = LengthDelimitedCodec<BincodeCodec<NewOrderRequest>>;

/// 成交通知编解码器
pub type TradeCodec = LengthDelimitedCodec<BincodeCodec<TradeNotification>>;

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Serialize, Deserialize, PartialEq)]
    struct TestMessage {
        id: u64,
        data: String,
    }

    #[test]
    fn test_bincode_codec() {
        let mut codec = BincodeCodec::<TestMessage>::new();

        let msg = TestMessage {
            id: 123,
            data: "Hello".to_string(),
        };

        // 编码
        let mut buf = vec![0u8; 1024];
        let size = codec.encode(&msg, &mut buf).unwrap();

        // 解码
        let decoded = codec.decode(&buf[..size]).unwrap().unwrap();
        assert_eq!(decoded.id, 123);
        assert_eq!(decoded.data, "Hello");
    }

    #[test]
    fn test_length_delimited_codec() {
        let inner = BincodeCodec::<TestMessage>::new();
        let mut codec = LengthDelimitedCodec::new(inner);

        let msg = TestMessage {
            id: 456,
            data: "World".to_string(),
        };

        // 编码
        let mut buf = vec![0u8; 1024];
        let size = codec.encode(&msg, &mut buf).unwrap();

        // 验证长度前缀
        let len = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]);
        assert_eq!(len as usize, size - 4);

        // 解码
        let decoded = codec.decode(&buf[..size]).unwrap().unwrap();
        assert_eq!(decoded.id, 456);
        assert_eq!(decoded.data, "World");
    }

    #[test]
    fn test_partial_frame() {
        let inner = BincodeCodec::<TestMessage>::new();
        let mut codec = LengthDelimitedCodec::new(inner);

        // 只有2字节，不足以读取长度前缀
        let buf = [0u8; 2];
        let result = codec.decode(&buf).unwrap();
        assert!(result.is_none());
    }
}
