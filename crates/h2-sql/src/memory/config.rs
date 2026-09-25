use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// メモリ制御設定 (Phase 1 セーフティガード & Phase 3 実行メモリ制限)
#[derive(Debug, Clone)]
pub struct MemoryConfig {
    /// 1クエリ内でマテリアライズ可能な最大行数 (Phase 1 暫定セーフティガード)
    /// 0 の場合は無制限。デフォルト: 100,000 行
    max_materialized_rows: Arc<AtomicUsize>,
    /// クエリ実行時のオペレータ別ワークメモリ上限 (バイト単位, Phase 3)
    /// デフォルト: 4MB (4 * 1024 * 1024)
    work_mem: Arc<AtomicUsize>,
    /// システム全体のクエリ同時実行用メモリプール総枠 (デフォルト: 128MB)
    total_query_memory: Arc<AtomicUsize>,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            max_materialized_rows: Arc::new(AtomicUsize::new(100_000)),
            work_mem: Arc::new(AtomicUsize::new(4 * 1024 * 1024)),
            total_query_memory: Arc::new(AtomicUsize::new(128 * 1024 * 1024)),
        }
    }
}

impl MemoryConfig {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn max_materialized_rows(&self) -> usize {
        self.max_materialized_rows.load(Ordering::Relaxed)
    }

    pub fn set_max_materialized_rows(&self, rows: usize) {
        self.max_materialized_rows.store(rows, Ordering::Relaxed);
    }

    pub fn work_mem(&self) -> usize {
        self.work_mem.load(Ordering::Relaxed)
    }

    pub fn set_work_mem(&self, bytes: usize) {
        self.work_mem.store(bytes, Ordering::Relaxed);
    }

    pub fn total_query_memory(&self) -> usize {
        self.total_query_memory.load(Ordering::Relaxed)
    }

    pub fn set_total_query_memory(&self, bytes: usize) {
        self.total_query_memory.store(bytes, Ordering::Relaxed);
    }
}
