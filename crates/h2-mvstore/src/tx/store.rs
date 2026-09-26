use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::store::MVStore;
use crate::tx::lock::LockManager;
use crate::tx::transaction::Transaction;
use crate::tx::versioned_value::VersionedValue;
use h2_types::H2Result;

/// トランザクション管理を行う MVStore ラッパー
pub struct TransactionStore {
    mvstore: Arc<MVStore>,
    next_tx_id: AtomicU64,
    active_transactions: RwLock<HashMap<u64, u64>>, // tx_id -> snapshot_version
    lock_manager: Arc<LockManager>,
    lock_timeout_ms: AtomicU64,
}

impl TransactionStore {
    pub fn new(mvstore: Arc<MVStore>) -> Arc<Self> {
        Arc::new(Self {
            mvstore,
            next_tx_id: AtomicU64::new(1),
            active_transactions: RwLock::new(HashMap::new()),
            lock_manager: Arc::new(LockManager::new()),
            lock_timeout_ms: AtomicU64::new(1000), // デフォルト 1秒
        })
    }

    pub fn mvstore(&self) -> &Arc<MVStore> {
        &self.mvstore
    }

    pub fn lock_manager(&self) -> &Arc<LockManager> {
        &self.lock_manager
    }

    pub fn set_lock_timeout_ms(&self, ms: u64) {
        self.lock_timeout_ms.store(ms, Ordering::SeqCst);
    }

    pub fn lock_timeout(&self) -> Duration {
        Duration::from_millis(self.lock_timeout_ms.load(Ordering::SeqCst))
    }

    pub fn is_tx_active(&self, tx_id: u64) -> bool {
        self.active_transactions.read().contains_key(&tx_id)
    }

    /// 新規トランザクションを開始（スナップショット分離）
    pub fn begin(self: &Arc<Self>) -> Transaction {
        let tx_id = self.next_tx_id.fetch_add(1, Ordering::SeqCst);
        let snapshot_version = self.mvstore.current_version();

        self.active_transactions.write().insert(tx_id, snapshot_version);
        Transaction::new(tx_id, snapshot_version, Arc::clone(self))
    }

    pub(crate) fn remove_active_tx(&self, tx_id: u64) {
        self.active_transactions.write().remove(&tx_id);
        self.lock_manager.unregister_wait(tx_id);
        self.lock_manager.notify_lock_released(tx_id);
    }

    pub fn active_tx_count(&self) -> usize {
        self.active_transactions.read().len()
    }

    /// 現在活動中のトランザクションの最古のスナップショットバージョンを取得
    pub fn oldest_active_version(&self) -> u64 {
        let active = self.active_transactions.read();
        active.values().copied().min().unwrap_or_else(|| self.mvstore.current_version())
    }

    /// 指定マップのデッドタプル・不要履歴を刈り込む（Online Vacuum）
    /// 刈り込んだデッドタプル数を返す
    pub fn vacuum_map(&self, map_name: &str) -> H2Result<usize> {
        let horizon_version = self.oldest_active_version();
        let map = self.mvstore.open_map(map_name);
        let entries = map.scan_all();
        let mut pruned_count = 0;

        for entry in entries {
            if let Ok(mut vv) = VersionedValue::from_bytes(&entry.value) {
                if vv.is_dead(horizon_version) {
                    map.remove(&entry.key);
                    pruned_count += 1;
                } else if vv.prune_old_versions(horizon_version) {
                    let new_bytes = vv.to_bytes()?;
                    map.put(entry.key, new_bytes);
                }
            }
        }

        Ok(pruned_count)
    }
}
