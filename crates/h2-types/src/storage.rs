use serde::{Deserialize, Serialize};

/// ログシーケンス番号 (Log Sequence Number)
pub type Lsn = u64;

/// ページ識別子 (データセグメント番号 + ページ番号)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PageId {
    pub segment_id: u32,
    pub page_no: u32,
}

impl PageId {
    pub fn new(segment_id: u32, page_no: u32) -> Self {
        Self { segment_id, page_no }
    }
}

/// WAL ログ操作種別
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LogOpType {
    Put,
    Delete,
    Commit,
    Rollback,
    CreateMap,
    DropMap,
    TruncateMap,
}

/// WAL ログレコード (Mini-Transaction Delta)
/// コンピュートノードからストレージ層へ送信される不変ログ単位
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogRecord {
    pub lsn: Lsn,
    pub prev_lsn: Lsn,
    pub tx_id: u64,
    pub op_type: LogOpType,
    pub map_name: String,
    pub key: Vec<u8>,
    pub value: Option<Vec<u8>>, // None は DELETE / Drop 操作
    pub timestamp_ms: u64,
}

impl LogRecord {
    pub fn put(lsn: Lsn, prev_lsn: Lsn, tx_id: u64, map_name: impl Into<String>, key: Vec<u8>, value: Vec<u8>) -> Self {
        Self {
            lsn,
            prev_lsn,
            tx_id,
            op_type: LogOpType::Put,
            map_name: map_name.into(),
            key,
            value: Some(value),
            timestamp_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
        }
    }

    pub fn delete(lsn: Lsn, prev_lsn: Lsn, tx_id: u64, map_name: impl Into<String>, key: Vec<u8>) -> Self {
        Self {
            lsn,
            prev_lsn,
            tx_id,
            op_type: LogOpType::Delete,
            map_name: map_name.into(),
            key,
            value: None,
            timestamp_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
        }
    }

    pub fn commit(lsn: Lsn, prev_lsn: Lsn, tx_id: u64) -> Self {
        Self {
            lsn,
            prev_lsn,
            tx_id,
            op_type: LogOpType::Commit,
            map_name: String::new(),
            key: Vec::new(),
            value: None,
            timestamp_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
        }
    }

    pub fn rollback(lsn: Lsn, prev_lsn: Lsn, tx_id: u64) -> Self {
        Self {
            lsn,
            prev_lsn,
            tx_id,
            op_type: LogOpType::Rollback,
            map_name: String::new(),
            key: Vec::new(),
            value: None,
            timestamp_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
        }
    }
}

/// Primary から Read Replica へ通知される軽量キャッシュ無効化イベント
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheInvalidationEvent {
    pub lsn: Lsn,
    pub map_name: String,
    pub key: Option<Vec<u8>>, // None の場合はマップ全体無効化
    pub timestamp_ms: u64,
}

/// スプリットブレインを防ぐ単調増加フェンシングトークン
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Default)]
pub struct FencingToken(pub u64);

impl FencingToken {
    pub fn next(&self) -> Self {
        Self(self.0 + 1)
    }

    pub fn val(&self) -> u64 {
        self.0
    }
}
