use h2_types::{H2Error, H2Result};
use parking_lot::{Condvar, Mutex};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

struct WaitState {
    wait_for: HashMap<u64, u64>,
    wait_cells: HashMap<u64, (usize, Arc<WaitCell>)>,
}

/// トランザクション間のロック待機関係（Wait-For Graph）およびデッドロック検出器
pub struct LockManager {
    state: Mutex<WaitState>,
    stripes: Vec<Mutex<()>>,
}

struct WaitCell {
    released: Mutex<bool>,
    condvar: Condvar,
}

impl LockManager {
    pub fn new() -> Self {
        const STRIPE_COUNT: usize = 256;
        let mut stripes = Vec::with_capacity(STRIPE_COUNT);
        for _ in 0..STRIPE_COUNT {
            stripes.push(Mutex::new(()));
        }
        Self {
            state: Mutex::new(WaitState {
                wait_for: HashMap::new(),
                wait_cells: HashMap::new(),
            }),
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
        if h2_types::query_metrics::enabled() {
            if let Some(guard) = self.stripes[idx].try_lock() {
                return guard;
            }
            let start = Instant::now();
            let guard = self.stripes[idx].lock();
            h2_types::query_metrics::record_lock_wait(start.elapsed());
            guard
        } else {
            self.stripes[idx].lock()
        }
    }

    /// 待機エッジを追加。デッドロック（サイクル）を検知した場合は Err(H2Error::LockConflict) を返す
    pub fn register_wait(&self, waiter: u64, holder: u64) -> H2Result<()> {
        let mut state = self.state.lock();
        if Self::check_cycle(&state.wait_for, waiter, holder) {
            return Err(H2Error::LockConflict(format!(
                "Deadlock detected: transaction {} waiting on transaction {}, cancelled to break cycle",
                waiter, holder
            )));
        }
        state.wait_for.insert(waiter, holder);
        state
            .wait_cells
            .entry(holder)
            .and_modify(|(count, _)| *count += 1)
            .or_insert_with(|| {
                (
                    1,
                    Arc::new(WaitCell {
                        released: Mutex::new(false),
                        condvar: Condvar::new(),
                    }),
                )
            });
        Ok(())
    }

    /// 待機エッジを削除
    pub fn unregister_wait(&self, waiter: u64) {
        let mut state = self.state.lock();
        if let Some(holder) = state.wait_for.remove(&waiter) {
            if let std::collections::hash_map::Entry::Occupied(mut entry) = state.wait_cells.entry(holder) {
                let (count, _) = entry.get_mut();
                *count = count.saturating_sub(1);
                if *count == 0 {
                    entry.remove();
                }
            }
        }
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
    pub fn wait_timeout(&self, holder: u64, timeout: Duration) -> bool {
        let cell = {
            let state = self.state.lock();
            state.wait_cells.get(&holder).map(|(_, cell)| Arc::clone(cell))
        };
        let Some(cell) = cell else {
            return true;
        };
        let mut released = cell.released.lock();
        let start = h2_types::query_metrics::enabled().then(Instant::now);
        let waiting_since = Instant::now();
        while !*released {
            let remaining = timeout.saturating_sub(waiting_since.elapsed());
            if remaining.is_zero() {
                break;
            }
            cell.condvar.wait_for(&mut released, remaining);
        }
        if let Some(start) = start {
            h2_types::query_metrics::record_lock_wait(start.elapsed());
        }
        *released
    }

    /// 指定したトランザクションを待つスレッドだけに通知する。
    pub fn notify_lock_released(&self, holder: u64) {
        let cell = {
            let mut state = self.state.lock();
            state.wait_cells.remove(&holder).map(|(_, cell)| cell)
        };
        if let Some(cell) = cell {
            *cell.released.lock() = true;
            cell.condvar.notify_all();
        }
    }
}
