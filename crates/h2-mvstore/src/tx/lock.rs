use std::collections::{HashMap, HashSet};
use std::time::Duration;
use parking_lot::{Condvar, Mutex};
use h2_types::{H2Error, H2Result};

/// トランザクション間のロック待機関係（Wait-For Graph）およびデッドロック検出器
pub struct LockManager {
    /// waiter_tx_id -> holder_tx_id
    wait_for: Mutex<HashMap<u64, u64>>,
    wait_mutex: Mutex<()>,
    condvar: Condvar,
    stripes: Vec<Mutex<()>>,
}

impl LockManager {
    pub fn new() -> Self {
        const STRIPE_COUNT: usize = 256;
        let mut stripes = Vec::with_capacity(STRIPE_COUNT);
        for _ in 0..STRIPE_COUNT {
            stripes.push(Mutex::new(()));
        }
        Self {
            wait_for: Mutex::new(HashMap::new()),
            wait_mutex: Mutex::new(()),
            condvar: Condvar::new(),
            stripes,
        }
    }

    /// キー操作時の排他ストライプロックを取得
    pub fn lock_key(&self, map_name: &str, key: &[u8]) -> parking_lot::MutexGuard<'_, ()> {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        map_name.hash(&mut hasher);
        key.hash(&mut hasher);
        let idx = (hasher.finish() as usize) % self.stripes.len();
        self.stripes[idx].lock()
    }

    /// 待機エッジを追加。デッドロック（サイクル）を検知した場合は Err(H2Error::LockConflict) を返す
    pub fn register_wait(&self, waiter: u64, holder: u64) -> H2Result<()> {
        let mut wait_for = self.wait_for.lock();
        if Self::check_cycle(&wait_for, waiter, holder) {
            return Err(H2Error::LockConflict(format!(
                "Deadlock detected: transaction {} waiting on transaction {}, cancelled to break cycle",
                waiter, holder
            )));
        }
        wait_for.insert(waiter, holder);
        Ok(())
    }

    /// 待機エッジを削除
    pub fn unregister_wait(&self, waiter: u64) {
        let mut wait_for = self.wait_for.lock();
        wait_for.remove(&waiter);
    }

    /// Wait-For Graph における循環（サイクル）探索
    fn check_cycle(wait_for: &HashMap<u64, u64>, waiter: u64, holder: u64) -> bool {
        if waiter == holder {
            return true;
        }
        let mut curr = holder;
        let mut visited = HashSet::new();
        visited.insert(waiter);
        visited.insert(holder);

        while let Some(&next) = wait_for.get(&curr) {
            if next == waiter {
                return true;
            }
            if !visited.insert(next) {
                // 既存のサイクルを検出
                return true;
            }
            curr = next;
        }
        false
    }

    /// ロック解放を待機（Condvar）
    pub fn wait_timeout(&self, timeout: Duration) -> bool {
        let mut guard = self.wait_mutex.lock();
        let res = self.condvar.wait_for(&mut guard, timeout);
        !res.timed_out()
    }

    /// ロック解放通知を全待機スレッドにブロードキャスト
    pub fn notify_lock_released(&self) {
        let _guard = self.wait_mutex.lock();
        self.condvar.notify_all();
    }
}
