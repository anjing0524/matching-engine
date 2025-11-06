// 全局内存分配器：使用 jemalloc 提升性能
// jemalloc 在高并发场景下比系统分配器快 8-15%
#[cfg(not(target_env = "msvc"))]
use tikv_jemallocator::Jemalloc;

#[cfg(not(target_env = "msvc"))]
#[global_allocator]
static GLOBAL: Jemalloc = Jemalloc;

// 将所有模块声明为公共的，这样二进制文件、测试和基准测试都能访问它们
pub mod protocol;
pub mod orderbook;
pub mod engine;
pub mod network;
pub mod symbol_pool;
