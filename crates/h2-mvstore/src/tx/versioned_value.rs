use serde::{Deserialize, Serialize};

/// コミット済みバージョンのレコード
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionRecord {
    pub value: Option<Vec<u8>>, // None は DELETE（削除された状態）
    pub commit_version: u64,
}

/// 未コミットの変更レコード
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UncommittedRecord {
    pub tx_id: u64,
    pub value: Option<Vec<u8>>, // None は DELETE
}

/// キーごとに保持されるマルチバージョン値（MVCC）
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionedValue {
    pub uncommitted: Option<UncommittedRecord>,
    pub committed_history: Vec<VersionRecord>, // 新しい順（降順）
}

impl VersionedValue {
    pub fn new_committed(value: Vec<u8>, commit_version: u64) -> Self {
        Self {
            uncommitted: None,
            committed_history: vec![VersionRecord {
                value: Some(value),
                commit_version,
            }],
        }
    }

    pub fn new_uncommitted(tx_id: u64, value: Option<Vec<u8>>) -> Self {
        Self {
            uncommitted: Some(UncommittedRecord { tx_id, value }),
            committed_history: Vec::new(),
        }
    }

    /// スナップショット分離（Snapshot Isolation）における可視性判定と値の取得
    pub fn read_visible(&self, reader_tx_id: u64, snapshot_version: u64) -> Option<&[u8]> {
        // 1. 自トランザクションの未コミット変更があればそれを返す
        if let Some(uncommitted) = &self.uncommitted {
            if uncommitted.tx_id == reader_tx_id {
                return uncommitted.value.as_deref();
            }
        }

        // 2. コミット履歴の中から snapshot_version 以下の最新の確定値を探す
        for record in &self.committed_history {
            if record.commit_version <= snapshot_version {
                return record.value.as_deref();
            }
        }

        None
    }

    /// 現在書き込み中（未コミット）のトランザクションIDを取得
    pub fn active_tx_id(&self) -> Option<u64> {
        self.uncommitted.as_ref().map(|u| u.tx_id)
    }

    /// 未コミットの変更を確定（コミット）
    pub fn commit_uncommitted(&mut self, commit_version: u64) {
        if let Some(uncommitted) = self.uncommitted.take() {
            self.committed_history.insert(
                0,
                VersionRecord {
                    value: uncommitted.value,
                    commit_version,
                },
            );
            // 履歴が長くなりすぎないよう最新32世代までに制限
            if self.committed_history.len() > 32 {
                self.committed_history.truncate(32);
            }
        }
    }

    /// 未コミットの変更を破棄（ロールバック）
    pub fn rollback_uncommitted(&mut self) {
        self.uncommitted = None;
    }
}
