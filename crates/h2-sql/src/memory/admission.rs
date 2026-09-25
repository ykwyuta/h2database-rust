use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use parking_lot::{Condvar, Mutex};

use h2_types::{H2Error, H2Result};

/// SQL Server 方式 Admission Control (Phase 3: Memory Grant Coordinator)
///
/// クエリ実行前に推定メモリを予約（Grant）し、システム全体のメモリプレッシャーを回避。
/// 上限超過時はクエリをキューイング待機させ、先行クエリ終了時に自動再開する。
pub struct MemoryGrantCoordinator {
    total_memory: usize,
    reserved_memory: AtomicUsize,
    lock: Mutex<()>,
    condvar: Condvar,
}

impl MemoryGrantCoordinator {
    pub fn new(total_memory: usize) -> Arc<Self> {
        Arc::new(Self {
            total_memory,
            reserved_memory: AtomicUsize::new(0),
            lock: Mutex::new(()),
            condvar: Condvar::new(),
        })
    }

    /// クエリ実行前のメモリ予約 (Memory Grant)
    pub fn acquire_grant(
        self: &Arc<Self>,
        requested_bytes: usize,
        timeout: Duration,
    ) -> H2Result<MemoryGrant> {
        let requested = requested_bytes.min(self.total_memory);
        let start = Instant::now();
        let mut guard = self.lock.lock();

        loop {
            let current = self.reserved_memory.load(Ordering::SeqCst);
            if current + requested <= self.total_memory {
                self.reserved_memory.fetch_add(requested, Ordering::SeqCst);
                return Ok(MemoryGrant {
                    coordinator: Arc::clone(self),
                    granted_bytes: requested,
                });
            }

            // タイムアウト判定
            let elapsed = start.elapsed();
            if elapsed >= timeout {
                return Err(H2Error::Execution(format!(
                    "Memory grant timeout: requested {} bytes, but system pool ({} bytes, reserved {} bytes) was congested",
                    requested, self.total_memory, current
                )));
            }

            let wait_time = timeout - elapsed;
            let result = self.condvar.wait_for(&mut guard, wait_time);
            if result.timed_out() && self.reserved_memory.load(Ordering::SeqCst) + requested > self.total_memory {
                return Err(H2Error::Execution(format!(
                    "Memory grant timeout after waiting {:?}", timeout
                )));
            }
        }
    }

    /// 予約メモリの返還
    pub fn release_grant(&self, bytes: usize) {
        self.reserved_memory.fetch_sub(bytes, Ordering::SeqCst);
        let _guard = self.lock.lock();
        self.condvar.notify_all();
    }

    pub fn total_memory(&self) -> usize {
        self.total_memory
    }

    pub fn reserved_memory(&self) -> usize {
        self.reserved_memory.load(Ordering::Relaxed)
    }

    pub fn available_memory(&self) -> usize {
        self.total_memory.saturating_sub(self.reserved_memory())
    }
}

/// クエリ実行スコープでメモリ枠を保持する RAII ガード
pub struct MemoryGrant {
    coordinator: Arc<MemoryGrantCoordinator>,
    pub granted_bytes: usize,
}

impl Drop for MemoryGrant {
    fn drop(&mut self) {
        self.coordinator.release_grant(self.granted_bytes);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_memory_grant_coordinator_basic() {
        let coordinator = MemoryGrantCoordinator::new(1024 * 1024); // 1MB プール

        let grant1 = coordinator.acquire_grant(512 * 1024, Duration::from_millis(100)).unwrap();
        assert_eq!(coordinator.reserved_memory(), 512 * 1024);
        assert_eq!(coordinator.available_memory(), 512 * 1024);

        let grant2 = coordinator.acquire_grant(256 * 1024, Duration::from_millis(100)).unwrap();
        assert_eq!(coordinator.reserved_memory(), 768 * 1024);

        // grant1 をドロップすると即座に空きが回復する
        drop(grant1);
        assert_eq!(coordinator.reserved_memory(), 256 * 1024);

        drop(grant2);
        assert_eq!(coordinator.reserved_memory(), 0);
    }

    #[test]
    fn test_memory_grant_timeout_and_queue() {
        let coordinator = MemoryGrantCoordinator::new(100);

        let grant1 = coordinator.acquire_grant(80, Duration::from_millis(50)).unwrap();
        // 80バイト使用中、残りは20バイト。50バイトを要求するとタイムアウトする
        let res = coordinator.acquire_grant(50, Duration::from_millis(20));
        assert!(res.is_err());

        drop(grant1);
        // 解放後は取得可能
        let grant2 = coordinator.acquire_grant(50, Duration::from_millis(50)).unwrap();
        assert_eq!(grant2.granted_bytes, 50);
    }
}
