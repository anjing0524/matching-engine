/// 零拷贝缓冲区实现

use super::traits::ZeroCopyBuffer;
use std::sync::Arc;

/// 共享缓冲区（基于Arc的零拷贝）
///
/// 适用于Tokio后端，使用Arc实现零拷贝clone
pub struct SharedBuffer {
    inner: Arc<Vec<u8>>,
    offset: usize,
    len: usize,
}

impl SharedBuffer {
    /// 从Vec创建
    pub fn from_vec(data: Vec<u8>) -> Self {
        let len = data.len();
        Self {
            inner: Arc::new(data),
            offset: 0,
            len,
        }
    }

    /// 创建指定大小的缓冲区
    pub fn with_capacity(capacity: usize) -> Self {
        Self::from_vec(vec![0u8; capacity])
    }

    /// 创建切片视图（零拷贝）
    pub fn slice(&self, start: usize, end: usize) -> Self {
        assert!(end <= self.len);
        assert!(start <= end);

        Self {
            inner: Arc::clone(&self.inner),
            offset: self.offset + start,
            len: end - start,
        }
    }
}

impl ZeroCopyBuffer for SharedBuffer {
    fn as_slice(&self) -> &[u8] {
        &self.inner[self.offset..self.offset + self.len]
    }

    fn as_mut_slice(&mut self) -> &mut [u8] {
        // 只有当Arc引用计数为1时才允许可变访问
        let inner = Arc::get_mut(&mut self.inner)
            .expect("Cannot get mutable access: buffer is shared");
        &mut inner[self.offset..self.offset + self.len]
    }

    fn len(&self) -> usize {
        self.len
    }

    fn clone_ref(&self) -> Arc<dyn ZeroCopyBuffer> {
        Arc::new(Self {
            inner: Arc::clone(&self.inner),
            offset: self.offset,
            len: self.len,
        })
    }
}

/// 对齐缓冲区（用于DMA）
///
/// 确保缓冲区地址和大小都对齐到指定边界
pub struct AlignedBuffer {
    data: Vec<u8>,
    alignment: usize,
    offset: usize,
    len: usize,
}

impl AlignedBuffer {
    /// 创建对齐缓冲区
    ///
    /// # 参数
    /// * `size` - 缓冲区大小
    /// * `alignment` - 对齐边界（必须是2的幂）
    pub fn new(size: usize, alignment: usize) -> Self {
        assert!(alignment.is_power_of_two(), "Alignment must be power of 2");

        // 分配额外空间以保证对齐
        let total_size = size + alignment - 1;
        let mut data = vec![0u8; total_size];

        // 计算对齐偏移
        let addr = data.as_ptr() as usize;
        let offset = (alignment - (addr % alignment)) % alignment;

        Self {
            data,
            alignment,
            offset,
            len: size,
        }
    }

    /// 获取虚拟地址
    pub fn virt_addr(&self) -> usize {
        (self.data.as_ptr() as usize) + self.offset
    }
}

impl ZeroCopyBuffer for AlignedBuffer {
    fn as_slice(&self) -> &[u8] {
        &self.data[self.offset..self.offset + self.len]
    }

    fn as_mut_slice(&mut self) -> &mut [u8] {
        &mut self.data[self.offset..self.offset + self.len]
    }

    fn len(&self) -> usize {
        self.len
    }

    fn clone_ref(&self) -> Arc<dyn ZeroCopyBuffer> {
        // 对齐缓冲区不支持零拷贝克隆，需要深拷贝
        Arc::new(SharedBuffer::from_vec(self.as_slice().to_vec()))
    }
}

/// 缓冲区池
///
/// 预分配缓冲区池，避免运行时分配
pub struct BufferPool {
    /// 空闲缓冲区栈
    free_list: parking_lot::Mutex<Vec<Box<dyn ZeroCopyBuffer>>>,
    /// 缓冲区大小
    buffer_size: usize,
    /// 池容量
    capacity: usize,
}

impl BufferPool {
    /// 创建缓冲区池
    pub fn new(buffer_size: usize, capacity: usize) -> Self {
        let mut free_list = Vec::with_capacity(capacity);

        // 预分配所有缓冲区
        for _ in 0..capacity {
            let buf = Box::new(SharedBuffer::with_capacity(buffer_size))
                as Box<dyn ZeroCopyBuffer>;
            free_list.push(buf);
        }

        Self {
            free_list: parking_lot::Mutex::new(free_list),
            buffer_size,
            capacity,
        }
    }

    /// 从池中分配缓冲区
    pub fn alloc(&self) -> Option<Box<dyn ZeroCopyBuffer>> {
        self.free_list.lock().pop()
    }

    /// 归还缓冲区到池
    pub fn free(&self, buf: Box<dyn ZeroCopyBuffer>) {
        let mut list = self.free_list.lock();
        if list.len() < self.capacity {
            list.push(buf);
        }
        // 如果池已满，丢弃缓冲区
    }

    /// 可用缓冲区数量
    pub fn available(&self) -> usize {
        self.free_list.lock().len()
    }

    /// 池容量
    pub fn capacity(&self) -> usize {
        self.capacity
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_shared_buffer() {
        let buf = SharedBuffer::from_vec(vec![1, 2, 3, 4, 5]);
        assert_eq!(buf.len(), 5);
        assert_eq!(buf.as_slice(), &[1, 2, 3, 4, 5]);

        // 零拷贝切片
        let slice = buf.slice(1, 4);
        assert_eq!(slice.as_slice(), &[2, 3, 4]);

        // 引用计数为2，原始缓冲区仍然有效
        assert_eq!(Arc::strong_count(&buf.inner), 2);
    }

    #[test]
    fn test_aligned_buffer() {
        let buf = AlignedBuffer::new(1024, 64);
        assert_eq!(buf.len(), 1024);

        // 验证对齐
        let addr = buf.virt_addr();
        assert_eq!(addr % 64, 0, "Buffer is not aligned to 64 bytes");
    }

    #[test]
    fn test_buffer_pool() {
        let pool = BufferPool::new(1024, 10);
        assert_eq!(pool.available(), 10);

        // 分配
        let buf1 = pool.alloc().unwrap();
        assert_eq!(pool.available(), 9);

        // 归还
        pool.free(buf1);
        assert_eq!(pool.available(), 10);
    }
}
