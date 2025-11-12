/// 高性能位图索引 - 使用硬件指令
///
/// 基于Vec<u64>实现，每个块存储64个bit
/// 使用CPU硬件指令 leading_zeros/trailing_zeros 实现O(1)查找
///
/// 性能特点：
/// - find_last_one: O(n/64) 使用 leading_zeros (BSR/CLZ指令)
/// - find_first_one: O(n/64) 使用 trailing_zeros (BSF/CTZ指令)
/// - set/clear: O(1) 位操作
///
/// 对于6000个价格层：
/// - 仅需94个u64块
/// - 最坏情况94次比较 vs 6000次比较
/// - 性能提升: ~64x

#[derive(Clone, Debug)]
pub struct FastBitmap {
    /// u64块数组，每块存储64个bit
    blocks: Vec<u64>,
    /// 总bit数
    len: usize,
}

impl FastBitmap {
    /// 创建新的位图
    ///
    /// # 参数
    /// * `len` - bit总数
    pub fn new(len: usize) -> Self {
        let num_blocks = (len + 63) / 64; // 向上取整
        Self {
            blocks: vec![0u64; num_blocks],
            len,
        }
    }

    /// 设置指定位置的bit
    ///
    /// # 参数
    /// * `index` - bit位置 (0-based)
    /// * `value` - true设置为1，false设置为0
    #[inline]
    pub fn set(&mut self, index: usize, value: bool) {
        debug_assert!(index < self.len, "Index out of bounds");

        let block_idx = index / 64;
        let bit_offset = index % 64;

        if value {
            // 设置为1
            self.blocks[block_idx] |= 1u64 << bit_offset;
        } else {
            // 设置为0
            self.blocks[block_idx] &= !(1u64 << bit_offset);
        }
    }

    /// 获取指定位置的bit
    #[inline]
    pub fn get(&self, index: usize) -> bool {
        debug_assert!(index < self.len, "Index out of bounds");

        let block_idx = index / 64;
        let bit_offset = index % 64;

        (self.blocks[block_idx] & (1u64 << bit_offset)) != 0
    }

    /// 查找最后一个设置的bit（最高位）- O(n/64)
    ///
    /// 用于买单最优价查找（从高到低）
    ///
    /// # 返回
    /// Some(index) - 最后一个为1的bit位置
    /// None - 所有bit都是0
    ///
    /// # 实现
    /// 从高到低遍历u64块，使用硬件指令leading_zeros
    ///
    /// # 性能
    /// - 最坏情况: O(num_blocks) = O(n/64)
    /// - 最好情况: O(1) (最后一个块有bit)
    /// - CPU指令: BSR (x86) / CLZ (ARM)
    #[inline]
    pub fn find_last_one(&self) -> Option<usize> {
        // 从高到低遍历u64块
        for (block_idx, &block) in self.blocks.iter().enumerate().rev() {
            if block != 0 {
                // 使用硬件指令找到最高位1
                // leading_zeros返回前导0的个数
                // 63 - leading_zeros = 最高位1的位置
                let bit_offset = 63 - block.leading_zeros() as usize;
                let index = block_idx * 64 + bit_offset;

                // 边界检查
                if index < self.len {
                    return Some(index);
                }
            }
        }
        None
    }

    /// 查找第一个设置的bit（最低位）- O(n/64)
    ///
    /// 用于卖单最优价查找（从低到高）
    ///
    /// # 返回
    /// Some(index) - 第一个为1的bit位置
    /// None - 所有bit都是0
    ///
    /// # 实现
    /// 从低到高遍历u64块，使用硬件指令trailing_zeros
    ///
    /// # 性能
    /// - 最坏情况: O(num_blocks) = O(n/64)
    /// - 最好情况: O(1) (第一个块有bit)
    /// - CPU指令: BSF (x86) / CTZ (ARM)
    #[inline]
    pub fn find_first_one(&self) -> Option<usize> {
        // 从低到高遍历u64块
        for (block_idx, &block) in self.blocks.iter().enumerate() {
            if block != 0 {
                // 使用硬件指令找到最低位1
                // trailing_zeros返回尾部0的个数
                let bit_offset = block.trailing_zeros() as usize;
                let index = block_idx * 64 + bit_offset;

                // 边界检查
                if index < self.len {
                    return Some(index);
                }
            }
        }
        None
    }

    /// 从指定位置向高位查找下一个设置的bit
    ///
    /// # 参数
    /// * `start` - 起始位置（不包含）
    ///
    /// # 返回
    /// Some(index) - 下一个为1的bit位置 (index > start)
    /// None - 没有找到
    #[inline]
    pub fn find_next_one(&self, start: usize) -> Option<usize> {
        if start + 1 >= self.len {
            return None;
        }

        let next_index = start + 1;
        let start_block = next_index / 64;
        let start_bit = next_index % 64;

        // 检查起始块的剩余部分
        if start_bit > 0 {
            let mask = !((1u64 << start_bit) - 1); // 掩码：从start_bit开始的所有位
            let masked = self.blocks[start_block] & mask;
            if masked != 0 {
                let bit_offset = masked.trailing_zeros() as usize;
                let index = start_block * 64 + bit_offset;
                if index < self.len {
                    return Some(index);
                }
            }
        }

        // 检查后续块
        for block_idx in (start_block + 1)..self.blocks.len() {
            let block = self.blocks[block_idx];
            if block != 0 {
                let bit_offset = block.trailing_zeros() as usize;
                let index = block_idx * 64 + bit_offset;
                if index < self.len {
                    return Some(index);
                }
            }
        }

        None
    }

    /// 从指定位置向低位查找上一个设置的bit
    ///
    /// # 参数
    /// * `start` - 起始位置（不包含）
    ///
    /// # 返回
    /// Some(index) - 上一个为1的bit位置 (index < start)
    /// None - 没有找到
    #[inline]
    pub fn find_prev_one(&self, start: usize) -> Option<usize> {
        if start == 0 {
            return None;
        }

        let prev_index = start - 1;
        let start_block = prev_index / 64;
        let start_bit = prev_index % 64;

        // 检查起始块的前面部分
        let mask = (1u64 << (start_bit + 1)) - 1; // 掩码：到start_bit的所有位
        let masked = self.blocks[start_block] & mask;
        if masked != 0 {
            let bit_offset = 63 - masked.leading_zeros() as usize;
            return Some(start_block * 64 + bit_offset);
        }

        // 检查前面的块
        for block_idx in (0..start_block).rev() {
            let block = self.blocks[block_idx];
            if block != 0 {
                let bit_offset = 63 - block.leading_zeros() as usize;
                return Some(block_idx * 64 + bit_offset);
            }
        }

        None
    }

    /// 返回位图总长度
    #[inline]
    pub fn len(&self) -> usize {
        self.len
    }

    /// 是否为空（所有bit都是0）
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.blocks.iter().all(|&block| block == 0)
    }

    /// 清空所有bit
    pub fn clear(&mut self) {
        for block in &mut self.blocks {
            *block = 0;
        }
    }

    /// 统计设置的bit数量
    pub fn count_ones(&self) -> usize {
        self.blocks.iter().map(|&block| block.count_ones() as usize).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_operations() {
        let mut bitmap = FastBitmap::new(128);

        // 初始状态
        assert_eq!(bitmap.find_first_one(), None);
        assert_eq!(bitmap.find_last_one(), None);

        // 设置一些bit
        bitmap.set(0, true);
        bitmap.set(63, true);
        bitmap.set(64, true);
        bitmap.set(127, true);

        assert_eq!(bitmap.get(0), true);
        assert_eq!(bitmap.get(63), true);
        assert_eq!(bitmap.get(64), true);
        assert_eq!(bitmap.get(127), true);
        assert_eq!(bitmap.get(1), false);

        // 查找
        assert_eq!(bitmap.find_first_one(), Some(0));
        assert_eq!(bitmap.find_last_one(), Some(127));

        // 清除
        bitmap.set(127, false);
        assert_eq!(bitmap.find_last_one(), Some(64));
    }

    #[test]
    fn test_find_next_prev() {
        let mut bitmap = FastBitmap::new(200);

        bitmap.set(10, true);
        bitmap.set(50, true);
        bitmap.set(100, true);
        bitmap.set(150, true);

        // find_next_one
        assert_eq!(bitmap.find_next_one(0), Some(10));
        assert_eq!(bitmap.find_next_one(10), Some(50));
        assert_eq!(bitmap.find_next_one(50), Some(100));
        assert_eq!(bitmap.find_next_one(100), Some(150));
        assert_eq!(bitmap.find_next_one(150), None);

        // find_prev_one
        assert_eq!(bitmap.find_prev_one(150), Some(100));
        assert_eq!(bitmap.find_prev_one(100), Some(50));
        assert_eq!(bitmap.find_prev_one(50), Some(10));
        assert_eq!(bitmap.find_prev_one(10), None);
    }

    #[test]
    fn test_large_bitmap() {
        let mut bitmap = FastBitmap::new(6000);

        // 模拟稀疏订单簿（只有10个活跃价格）
        bitmap.set(100, true);
        bitmap.set(1000, true);
        bitmap.set(2000, true);
        bitmap.set(3000, true);
        bitmap.set(4000, true);
        bitmap.set(5000, true);
        bitmap.set(5500, true);
        bitmap.set(5800, true);
        bitmap.set(5900, true);
        bitmap.set(5999, true);

        // 查找最优价应该非常快
        assert_eq!(bitmap.find_first_one(), Some(100));
        assert_eq!(bitmap.find_last_one(), Some(5999));

        // 统计
        assert_eq!(bitmap.count_ones(), 10);
    }

    #[test]
    fn test_hardware_instructions() {
        // 验证硬件指令正确性
        let block = 0b1010_0000_0000_0000_0000_0000_0000_0000_0000_0000_0000_0000_0000_0000_0000_0000u64;

        // leading_zeros: 前导0的个数
        assert_eq!(block.leading_zeros(), 0); // 最高位是1

        // trailing_zeros: 尾部0的个数
        assert_eq!(block.trailing_zeros(), 61); // 最低位1在第61位

        let block2 = 0b0000_0000_0000_0000_0000_0000_0000_0000_0000_0000_0000_0000_0000_0000_0000_0001u64;
        assert_eq!(block2.leading_zeros(), 63);
        assert_eq!(block2.trailing_zeros(), 0);
    }
}
