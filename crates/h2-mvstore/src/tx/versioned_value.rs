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

    pub fn to_bytes(&self) -> h2_types::H2Result<Vec<u8>> {
        bincode::serialize(self).map_err(|e| h2_types::H2Error::Serialization(e.to_string()))
    }

    pub fn from_bytes(bytes: &[u8]) -> h2_types::H2Result<Self> {
        if bytes.is_empty() {
            return Err(h2_types::H2Error::Serialization("Empty versioned value bytes".to_string()));
        }
        bincode::deserialize::<VersionedValue>(bytes)
            .map_err(|e| h2_types::H2Error::Serialization(e.to_string()))
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

    /// 指定世代（horizon_version）以下のトランザクションにおいて完全にデッドタプル（削除済み）か判定
    pub fn is_dead(&self, horizon_version: u64) -> bool {
        if self.uncommitted.is_some() {
            return false;
        }
        if self.committed_history.is_empty() {
            return true;
        }
        if let Some(first) = self.committed_history.first() {
            if first.commit_version <= horizon_version && first.value.is_none() {
                return true;
            }
        }
        false
    }

    /// horizon_version より古い余分な履歴バージョンを刈り込む。変更があった場合 true を返す
    pub fn prune_old_versions(&mut self, horizon_version: u64) -> bool {
        if self.committed_history.len() <= 1 {
            return false;
        }
        let mut cutoff = None;
        for (i, rec) in self.committed_history.iter().enumerate() {
            if rec.commit_version <= horizon_version {
                cutoff = Some(i + 1);
                break;
            }
        }
        if let Some(cutoff_idx) = cutoff {
            if cutoff_idx < self.committed_history.len() {
                self.committed_history.truncate(cutoff_idx);
                return true;
            }
        }
        false
    }

    /// 高速ゼロコピー可視性判定（単一コミット世代の典型パターンをゼロアロケーションで判定）
    #[inline(always)]
    pub fn read_visible_raw(bytes: &[u8], _reader_tx_id: u64, snapshot_version: u64) -> Option<&[u8]> {
        if bytes.len() >= 26 && bytes[0] == 0 {
            // uncommitted is None
            if let Ok(hist_len_bytes) = bytes[1..9].try_into() {
                let hist_len = u64::from_le_bytes(hist_len_bytes);
                if hist_len == 1 && bytes[9] == 1 {
                    // committed_history.len() == 1, value is Some
                    if let Ok(val_len_bytes) = bytes[10..18].try_into() {
                        let val_len = u64::from_le_bytes(val_len_bytes) as usize;
                        if bytes.len() == 18 + val_len + 8 {
                            if let Ok(commit_ver_bytes) = bytes[18 + val_len..26 + val_len].try_into() {
                                let commit_ver = u64::from_le_bytes(commit_ver_bytes);
                                if commit_ver <= snapshot_version {
                                    return Some(&bytes[18..18 + val_len]);
                                }
                            }
                        }
                    }
                }
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_read_visible_raw_fast_path() {
        let payload = b"fast path test payload 12345";
        let vv = VersionedValue::new_committed(payload.to_vec(), 10);
        let bytes = vv.to_bytes().unwrap();

        // 正常判定 (snapshot_version >= commit_version)
        let visible = VersionedValue::read_visible_raw(&bytes, 1, 10);
        assert_eq!(visible, Some(payload.as_slice()));

        let visible2 = VersionedValue::read_visible_raw(&bytes, 1, 20);
        assert_eq!(visible2, Some(payload.as_slice()));

        // 未来のバージョンは非可視
        let invisible = VersionedValue::read_visible_raw(&bytes, 1, 5);
        assert_eq!(invisible, None);
    }
}
