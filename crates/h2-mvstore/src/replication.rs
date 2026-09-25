use serde::{Deserialize, Serialize};
use h2_types::H2Result;

/// マップに対する個別の変更操作
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReplicationChange {
    pub map_name: String,
    pub key: Vec<u8>,
    pub value: Option<Vec<u8>>,
    pub is_clear: bool,
}

impl ReplicationChange {
    pub fn put(map_name: impl Into<String>, key: Vec<u8>, value: Vec<u8>) -> Self {
        Self {
            map_name: map_name.into(),
            key,
            value: Some(value),
            is_clear: false,
        }
    }

    pub fn remove(map_name: impl Into<String>, key: Vec<u8>) -> Self {
        Self {
            map_name: map_name.into(),
            key,
            value: None,
            is_clear: false,
        }
    }

    pub fn clear(map_name: impl Into<String>) -> Self {
        Self {
            map_name: map_name.into(),
            key: Vec::new(),
            value: None,
            is_clear: true,
        }
    }
}

/// 1回のコミットで確定した変更レコードのバッチ
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplicationCommitRecord {
    pub commit_version: u64,
    pub changes: Vec<ReplicationChange>,
    pub timestamp_ms: u64,
}

/// マップ操作をリアルタイムに記録するシンクトレイト
pub trait MapChangeSink: Send + Sync {
    fn record_put(&self, map_name: &str, key: Vec<u8>, value: Vec<u8>);
    fn record_remove(&self, map_name: &str, key: Vec<u8>);
    fn record_clear(&self, map_name: &str);
    fn drain_changes(&self) -> Vec<ReplicationChange>;
}

/// デフォルトのインメモリ変更コレクター
#[derive(Default)]
pub struct DefaultChangeCollector {
    changes: parking_lot::Mutex<Vec<ReplicationChange>>,
}

impl DefaultChangeCollector {
    pub fn new() -> Self {
        Self {
            changes: parking_lot::Mutex::new(Vec::new()),
        }
    }
}

impl MapChangeSink for DefaultChangeCollector {
    fn record_put(&self, map_name: &str, key: Vec<u8>, value: Vec<u8>) {
        self.changes.lock().push(ReplicationChange::put(map_name, key, value));
    }

    fn record_remove(&self, map_name: &str, key: Vec<u8>) {
        self.changes.lock().push(ReplicationChange::remove(map_name, key));
    }

    fn record_clear(&self, map_name: &str) {
        self.changes.lock().push(ReplicationChange::clear(map_name));
    }

    fn drain_changes(&self) -> Vec<ReplicationChange> {
        let mut guard = self.changes.lock();
        std::mem::take(&mut *guard)
    }
}

/// コミット発生時に通知を受け取り、レプリケーションを制御するリスナートレイト
pub trait ReplicationListener: Send + Sync {
    /// コミット発生時に呼び出される。
    /// remote_apply モードの場合、スタンバイで適用完了するまでこのメソッド呼び出しがブロックする。
    fn on_commit(&self, commit_version: u64, changes: Vec<ReplicationChange>) -> H2Result<()>;
}
