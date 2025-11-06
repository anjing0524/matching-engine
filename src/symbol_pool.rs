/// Symbol字符串池 - 用于高效管理交易对符号
///
/// 设计目标：
/// 1. 避免重复创建Arc<str>，减少堆分配
/// 2. 使用读写锁实现高并发访问
/// 3. 常见符号仅创建一次，后续仅克隆Arc（原子增量）
///
/// 性能特点：
/// - 首次访问：~100-200ns（写锁 + 堆分配）
/// - 后续访问：~10-20ns（读锁 + Arc克隆）
/// - 预期性能提升：15-20%

use std::sync::Arc;
use std::collections::HashMap;
use parking_lot::RwLock;

/// 全局符号池，用于intern交易对符号字符串
pub struct SymbolPool {
    /// 内部存储：String -> Arc<str>映射
    /// 使用parking_lot::RwLock提供更好的性能
    symbols: RwLock<HashMap<String, Arc<str>>>,
}

impl SymbolPool {
    /// 创建新的符号池
    pub fn new() -> Self {
        Self {
            symbols: RwLock::new(HashMap::new()),
        }
    }

    /// 创建带有预设容量的符号池
    ///
    /// # 参数
    /// * `capacity` - 预期的唯一符号数量
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            symbols: RwLock::new(HashMap::with_capacity(capacity)),
        }
    }

    /// Intern一个符号字符串，返回共享的Arc<str>
    ///
    /// # 性能特点
    /// - 如果符号已存在：仅读锁 + Arc克隆（~10-20ns）
    /// - 如果符号不存在：读锁 + 写锁 + 堆分配（~100-200ns）
    ///
    /// # 示例
    /// ```
    /// let pool = SymbolPool::new();
    /// let btc1 = pool.intern("BTC/USD");
    /// let btc2 = pool.intern("BTC/USD");
    /// assert!(Arc::ptr_eq(&btc1, &btc2)); // 指向同一内存
    /// ```
    #[inline]
    pub fn intern(&self, symbol: &str) -> Arc<str> {
        // 快速路径：读锁查找（无竞争）
        {
            let read_guard = self.symbols.read();
            if let Some(arc) = read_guard.get(symbol) {
                return arc.clone(); // 仅原子增量，极快
            }
        }

        // 慢速路径：需要插入新符号（首次访问）
        let mut write_guard = self.symbols.write();

        // Double-check：可能在获取写锁期间其他线程已插入
        write_guard.entry(symbol.to_string())
            .or_insert_with(|| Arc::from(symbol))
            .clone()
    }

    /// 预加载常见符号到池中
    ///
    /// 适用场景：启动时预热常见交易对，避免运行时写锁
    ///
    /// # 示例
    /// ```
    /// let pool = SymbolPool::new();
    /// pool.preload(&["BTC/USD", "ETH/USD", "BNB/USD"]);
    /// ```
    pub fn preload(&self, symbols: &[&str]) {
        let mut write_guard = self.symbols.write();
        for &symbol in symbols {
            write_guard.entry(symbol.to_string())
                .or_insert_with(|| Arc::from(symbol));
        }
    }

    /// 获取当前池中符号数量
    pub fn len(&self) -> usize {
        self.symbols.read().len()
    }

    /// 检查池是否为空
    pub fn is_empty(&self) -> bool {
        self.symbols.read().is_empty()
    }

    /// 清空符号池（主要用于测试）
    #[cfg(test)]
    pub fn clear(&self) {
        self.symbols.write().clear();
    }
}

impl Default for SymbolPool {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_intern_returns_same_arc() {
        let pool = SymbolPool::new();
        let sym1 = pool.intern("BTC/USD");
        let sym2 = pool.intern("BTC/USD");

        // 应该返回相同的Arc（指针相等）
        assert!(Arc::ptr_eq(&sym1, &sym2));
    }

    #[test]
    fn test_intern_different_symbols() {
        let pool = SymbolPool::new();
        let btc = pool.intern("BTC/USD");
        let eth = pool.intern("ETH/USD");

        // 不同符号应该有不同的Arc
        assert!(!Arc::ptr_eq(&btc, &eth));
        assert_eq!(btc.as_ref(), "BTC/USD");
        assert_eq!(eth.as_ref(), "ETH/USD");
    }

    #[test]
    fn test_preload() {
        let pool = SymbolPool::new();
        pool.preload(&["BTC/USD", "ETH/USD", "BNB/USD"]);

        assert_eq!(pool.len(), 3);

        // 预加载后访问应该返回相同的Arc
        let btc1 = pool.intern("BTC/USD");
        let btc2 = pool.intern("BTC/USD");
        assert!(Arc::ptr_eq(&btc1, &btc2));
    }

    #[test]
    fn test_concurrent_access() {
        use std::thread;

        let pool = Arc::new(SymbolPool::new());
        let mut handles = vec![];

        // 10个线程并发访问相同符号
        for _ in 0..10 {
            let pool_clone = pool.clone();
            let handle = thread::spawn(move || {
                for _ in 0..1000 {
                    let _ = pool_clone.intern("BTC/USD");
                }
            });
            handles.push(handle);
        }

        for handle in handles {
            handle.join().unwrap();
        }

        // 应该只创建一个Arc
        assert_eq!(pool.len(), 1);
    }
}
