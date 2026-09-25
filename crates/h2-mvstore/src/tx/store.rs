use parking_lot::RwLock;
use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::store::MVStore;
use crate::tx::lock::LockManager;
use crate::tx::transaction::Transaction;

/// トランザクション管理を行う MVStore ラッパー
pub struct TransactionStore {
    mvstore: Arc<MVStore>,
    next_tx_id: AtomicU64,
    active_transactions: RwLock<HashSet<u64>>,
    lock_manager: Arc<LockManager>,
    lock_timeout_ms: AtomicU64,
}

impl TransactionStore {
    pub fn new(mvstore: Arc<MVStore>) -> Arc<Self> {
        Arc::new(Self {
            mvstore,
            next_tx_id: AtomicU64::new(1),
            active_transactions: RwLock::new(HashSet::new()),
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
        self.active_transactions.read().contains(&tx_id)
    }

    /// 新規トランザクションを開始（スナップショット分離）
    pub fn begin(self: &Arc<Self>) -> Transaction {
        let tx_id = self.next_tx_id.fetch_add(1, Ordering::SeqCst);
        let snapshot_version = self.mvstore.current_version();

        self.active_transactions.write().insert(tx_id);
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
}
