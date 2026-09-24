use std::sync::Arc;
use parking_lot::RwLock;

use h2_types::{H2Error, H2Result};
use crate::tx::store::TransactionStore;
use crate::tx::versioned_value::{UncommittedRecord, VersionedValue};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransactionStatus {
    Open,
    Committed,
    RolledBack,
}

/// 単一キーに対する変更履歴（Undo Log用）
#[derive(Debug, Clone)]
pub struct UndoLogEntry {
    pub map_name: String,
    pub key: Vec<u8>,
}

/// MVCC トランザクション
pub struct Transaction {
    pub tx_id: u64,
    pub snapshot_version: u64,
    status: Arc<RwLock<TransactionStatus>>,
    undo_log: Arc<RwLock<Vec<UndoLogEntry>>>,
    store: Arc<TransactionStore>,
}

impl Transaction {
    pub fn new(
        tx_id: u64,
        snapshot_version: u64,
        store: Arc<TransactionStore>,
    ) -> Self {
        Self {
            tx_id,
            snapshot_version,
            status: Arc::new(RwLock::new(TransactionStatus::Open)),
            undo_log: Arc::new(RwLock::new(Vec::new())),
            store,
        }
    }

    pub fn status(&self) -> TransactionStatus {
        *self.status.read()
    }

    /// トランザクション分離に基づいてキーを読み取る
    pub fn get(&self, map_name: &str, key: &[u8]) -> H2Result<Option<Vec<u8>>> {
        self.check_open()?;
        let map = self.store.mvstore().open_map(map_name);
        let Some(val_bytes) = map.get(key) else {
            return Ok(None);
        };

        let vv: VersionedValue = serde_json::from_slice(&val_bytes)
            .map_err(|e| H2Error::Serialization(e.to_string()))?;

        Ok(vv.read_visible(self.tx_id, self.snapshot_version).map(|v| v.to_vec()))
    }

    /// トランザクション内でキーバリューを挿入/更新
    pub fn put(&self, map_name: &str, key: Vec<u8>, value: Vec<u8>) -> H2Result<()> {
        self.check_open()?;
        let map = self.store.mvstore().open_map(map_name);

        let mut vv = if let Some(val_bytes) = map.get(&key) {
            let existing: VersionedValue = serde_json::from_slice(&val_bytes)
                .map_err(|e| H2Error::Serialization(e.to_string()))?;

            // Write-Write Conflict チェック
            if let Some(other_tx_id) = existing.active_tx_id() {
                if other_tx_id != self.tx_id {
                    return Err(H2Error::LockConflict(format!(
                        "Key is locked by active transaction {}",
                        other_tx_id
                    )));
                }
            }
            existing
        } else {
            VersionedValue {
                uncommitted: None,
                committed_history: Vec::new(),
            }
        };

        vv.uncommitted = Some(UncommittedRecord {
            tx_id: self.tx_id,
            value: Some(value),
        });

        let serialized = serde_json::to_vec(&vv)
            .map_err(|e| H2Error::Serialization(e.to_string()))?;

        self.undo_log.write().push(UndoLogEntry {
            map_name: map_name.to_string(),
            key: key.clone(),
        });

        map.put(key, serialized);
        Ok(())
    }

    /// トランザクション内でキーを削除
    pub fn remove(&self, map_name: &str, key: &[u8]) -> H2Result<bool> {
        self.check_open()?;
        let map = self.store.mvstore().open_map(map_name);

        let Some(val_bytes) = map.get(key) else {
            return Ok(false);
        };

        let mut vv: VersionedValue = serde_json::from_slice(&val_bytes)
            .map_err(|e| H2Error::Serialization(e.to_string()))?;

        if let Some(other_tx_id) = vv.active_tx_id() {
            if other_tx_id != self.tx_id {
                return Err(H2Error::LockConflict(format!(
                    "Key is locked by active transaction {}",
                    other_tx_id
                )));
            }
        }

        // もし可視な値が存在しなければ削除対象なし
        if vv.read_visible(self.tx_id, self.snapshot_version).is_none() {
            return Ok(false);
        }

        vv.uncommitted = Some(UncommittedRecord {
            tx_id: self.tx_id,
            value: None, // 削除マーク
        });

        let serialized = serde_json::to_vec(&vv)
            .map_err(|e| H2Error::Serialization(e.to_string()))?;

        self.undo_log.write().push(UndoLogEntry {
            map_name: map_name.to_string(),
            key: key.to_vec(),
        });

        map.put(key.to_vec(), serialized);
        Ok(true)
    }

    /// 可視な全エントリを走査
    pub fn scan_visible(&self, map_name: &str) -> H2Result<Vec<(Vec<u8>, Vec<u8>)>> {
        self.check_open()?;
        let map = self.store.mvstore().open_map(map_name);
        let raw_entries = map.scan_all();

        let mut visible_entries = Vec::new();
        for entry in raw_entries {
            if let Ok(vv) = serde_json::from_slice::<VersionedValue>(&entry.value) {
                if let Some(val) = vv.read_visible(self.tx_id, self.snapshot_version) {
                    visible_entries.push((entry.key, val.to_vec()));
                }
            }
        }

        Ok(visible_entries)
    }

    /// コミット
    pub fn commit(&self) -> H2Result<()> {
        let mut status = self.status.write();
        if *status != TransactionStatus::Open {
            return Err(H2Error::Transaction("Transaction is already closed".to_string()));
        }

        let commit_version = self.store.mvstore().current_version() + 1;
        let undo_logs = self.undo_log.read();

        for log in undo_logs.iter() {
            let map = self.store.mvstore().open_map(&log.map_name);
            if let Some(val_bytes) = map.get(&log.key) {
                if let Ok(mut vv) = serde_json::from_slice::<VersionedValue>(&val_bytes) {
                    if vv.active_tx_id() == Some(self.tx_id) {
                        vv.commit_uncommitted(commit_version);
                        let serialized = serde_json::to_vec(&vv)
                            .map_err(|e| H2Error::Serialization(e.to_string()))?;
                        map.put(log.key.clone(), serialized);
                    }
                }
            }
        }

        *status = TransactionStatus::Committed;
        self.store.remove_active_tx(self.tx_id);
        self.store.mvstore().commit()?;
        Ok(())
    }

    /// ロールバック
    pub fn rollback(&self) -> H2Result<()> {
        let mut status = self.status.write();
        if *status != TransactionStatus::Open {
            return Err(H2Error::Transaction("Transaction is already closed".to_string()));
        }

        let mut undo_logs = self.undo_log.write();
        while let Some(log) = undo_logs.pop() {
            let map = self.store.mvstore().open_map(&log.map_name);
            if let Some(val_bytes) = map.get(&log.key) {
                if let Ok(mut vv) = serde_json::from_slice::<VersionedValue>(&val_bytes) {
                    if vv.active_tx_id() == Some(self.tx_id) {
                        vv.rollback_uncommitted();
                        if vv.committed_history.is_empty() {
                            map.remove(&log.key);
                        } else {
                            let serialized = serde_json::to_vec(&vv)
                                .map_err(|e| H2Error::Serialization(e.to_string()))?;
                            map.put(log.key, serialized);
                        }
                    }
                }
            }
        }

        *status = TransactionStatus::RolledBack;
        self.store.remove_active_tx(self.tx_id);
        Ok(())
    }

    fn check_open(&self) -> H2Result<()> {
        if *self.status.read() != TransactionStatus::Open {
            return Err(H2Error::Transaction("Transaction is not open".to_string()));
        }
        Ok(())
    }
}

impl Drop for Transaction {
    fn drop(&mut self) {
        if *self.status.read() == TransactionStatus::Open {
            let _ = self.rollback();
        }
    }
}
