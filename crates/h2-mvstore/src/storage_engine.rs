use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use h2_types::{FencingToken, H2Error, H2Result, LogOpType, LogRecord, Lsn, PageId};
use crate::store::MVStore;

/// コンピュート層とストレージ層を接続する統一抽象ストレージトレイト
pub trait StorageEngine: Send + Sync {
    /// 現在コミット済みの最新 LSN を取得
    fn latest_committed_lsn(&self) -> Lsn;

    /// 現在有効なリーダー世代トークン (Fencing Token)
    fn fencing_token(&self) -> FencingToken;

    /// 新しい Fencing Token でリーダーシップ（排他書き込みリース）を獲得
    fn acquire_lease(&self, token: FencingToken) -> H2Result<()>;

    /// WAL ログレコード群を追記 ("The Log is the Database")
    fn append_logs(&self, records: &[LogRecord], token: FencingToken) -> H2Result<Lsn>;

    /// 指定 LSN がクォーラムノード群に永続化されるまで同期待機
    fn wait_for_quorum_lsn(&self, lsn: Lsn) -> H2Result<()>;

    /// 特定マップからキーの値を取得（指定バージョン時点）
    fn get_key(&self, map_name: &str, key: &[u8], read_version: Lsn) -> H2Result<Option<Vec<u8>>>;

    /// 特定マップのキーレンジスキャン（指定バージョン時点）
    fn scan_range(
        &self,
        map_name: &str,
        start_key: Option<&[u8]>,
        end_key: Option<&[u8]>,
        read_version: Lsn,
    ) -> H2Result<Vec<(Vec<u8>, Vec<u8>)>>;

    /// ページ ID からバイナリページデータを直接取得
    fn get_page(&self, page_id: PageId, read_version: Lsn) -> H2Result<Vec<u8>>;

    /// 存在するすべてのマップ名を取得
    fn get_all_map_names(&self, read_version: Lsn) -> H2Result<Vec<String>>;
}

/// 従来のローカル組み込み MVStore を StorageEngine としてラップする実装
/// （ゼロオーバーヘッドで同一プロセス内で高速動作）
pub struct LocalMVStoreEngine {
    store: Arc<MVStore>,
    current_lsn: AtomicU64,
    fencing_token: parking_lot::RwLock<FencingToken>,
}

impl LocalMVStoreEngine {
    pub fn new(store: Arc<MVStore>) -> Self {
        let initial_lsn = store.current_version();
        Self {
            store,
            current_lsn: AtomicU64::new(initial_lsn),
            fencing_token: parking_lot::RwLock::new(FencingToken(1)),
        }
    }

    pub fn inner_store(&self) -> &Arc<MVStore> {
        &self.store
    }
}

impl StorageEngine for LocalMVStoreEngine {
    fn latest_committed_lsn(&self) -> Lsn {
        self.current_lsn.load(Ordering::SeqCst)
    }

    fn fencing_token(&self) -> FencingToken {
        *self.fencing_token.read()
    }

    fn acquire_lease(&self, token: FencingToken) -> H2Result<()> {
        let mut current = self.fencing_token.write();
        if token.0 < current.0 {
            return Err(H2Error::Storage(format!(
                "Fencing token rejection: provided token {} is older than active token {}",
                token.0, current.0
            )));
        }
        *current = token;
        Ok(())
    }

    fn append_logs(&self, records: &[LogRecord], token: FencingToken) -> H2Result<Lsn> {
        let current_token = *self.fencing_token.read();
        if token.0 < current_token.0 {
            return Err(H2Error::Storage(format!(
                "Fencing token violation: writer token {} < storage active token {}",
                token.0, current_token.0
            )));
        }

        let mut max_lsn = self.latest_committed_lsn();
        for record in records {
            max_lsn = max_lsn.max(record.lsn);
            match record.op_type {
                LogOpType::Put => {
                    if let Some(ref val) = record.value {
                        let map = self.store.open_map(&record.map_name);
                        map.put(record.key.clone(), val.clone());
                    }
                }
                LogOpType::Delete => {
                    let map = self.store.open_map(&record.map_name);
                    map.remove(&record.key);
                }
                LogOpType::TruncateMap => {
                    let map = self.store.open_map(&record.map_name);
                    map.clear();
                }
                LogOpType::Commit => {
                    let _ = self.store.commit();
                }
                LogOpType::Rollback => {
                    // ロールバック時は何もしないか、トランザクション側の巻き戻しを行う
                }
                LogOpType::CreateMap => {
                    let _ = self.store.open_map(&record.map_name);
                }
                LogOpType::DropMap => {
                    let map = self.store.open_map(&record.map_name);
                    map.clear();
                }
            }
        }

        self.current_lsn.store(max_lsn, Ordering::SeqCst);
        Ok(max_lsn)
    }

    fn wait_for_quorum_lsn(&self, _lsn: Lsn) -> H2Result<()> {
        // ローカルエンジンでは即座に fsync / メモリ反映されるため即座に成功
        Ok(())
    }

    fn get_key(&self, map_name: &str, key: &[u8], _read_version: Lsn) -> H2Result<Option<Vec<u8>>> {
        let map = self.store.open_map(map_name);
        Ok(map.get(key))
    }

    fn scan_range(
        &self,
        map_name: &str,
        start_key: Option<&[u8]>,
        end_key: Option<&[u8]>,
        _read_version: Lsn,
    ) -> H2Result<Vec<(Vec<u8>, Vec<u8>)>> {
        let map = self.store.open_map(map_name);
        let all = map.scan_all();
        let filtered = all
            .into_iter()
            .filter(|e| {
                if let Some(start) = start_key {
                    if e.key.as_slice() < start {
                        return false;
                    }
                }
                if let Some(end) = end_key {
                    if e.key.as_slice() > end {
                        return false;
                    }
                }
                true
            })
            .map(|e| (e.key, e.value))
            .collect();
        Ok(filtered)
    }

    fn get_page(&self, page_id: PageId, _read_version: Lsn) -> H2Result<Vec<u8>> {
        // 単純化したページ表現（セグメント + ページ番号のメタデータを返す）
        let mut data = Vec::with_capacity(16);
        data.extend_from_slice(&page_id.segment_id.to_be_bytes());
        data.extend_from_slice(&page_id.page_no.to_be_bytes());
        Ok(data)
    }

    fn get_all_map_names(&self, _read_version: Lsn) -> H2Result<Vec<String>> {
        Ok(self.store.get_map_names())
    }
}
