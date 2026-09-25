use std::sync::atomic::{AtomicUsize, Ordering};

/// クエリ実行時の動的メモリトラッカー (Phase 3 work_mem 監視)
pub struct MemoryTracker {
    limit_bytes: usize,
    used_bytes: AtomicUsize,
}

impl MemoryTracker {
    pub fn new(limit_bytes: usize) -> Self {
        Self {
            limit_bytes,
            used_bytes: AtomicUsize::new(0),
        }
    }

    /// メモリの割り当てを試行。上限 (limit_bytes) を超える場合は false を返す
    pub fn try_allocate(&self, bytes: usize) -> bool {
        let mut current = self.used_bytes.load(Ordering::Relaxed);
        loop {
            if current + bytes > self.limit_bytes {
                return false;
            }
            match self.used_bytes.compare_exchange_weak(
                current,
                current + bytes,
                Ordering::SeqCst,
                Ordering::Relaxed,
            ) {
                Ok(_) => return true,
                Err(actual) => current = actual,
            }
        }
    }

    /// メモリの割り当てを強制加算（超過時はエラー判定可能）
    pub fn allocate_force(&self, bytes: usize) -> usize {
        self.used_bytes.fetch_add(bytes, Ordering::SeqCst) + bytes
    }

    /// メモリの解放
    pub fn release(&self, bytes: usize) {
        let prev = self.used_bytes.fetch_sub(bytes, Ordering::SeqCst);
        if prev < bytes {
            self.used_bytes.store(0, Ordering::SeqCst);
        }
    }

    pub fn used_bytes(&self) -> usize {
        self.used_bytes.load(Ordering::Relaxed)
    }

    pub fn limit_bytes(&self) -> usize {
        self.limit_bytes
    }

    pub fn is_exceeded(&self) -> bool {
        self.used_bytes() > self.limit_bytes
    }
}
