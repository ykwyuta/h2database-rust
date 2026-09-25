use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use parking_lot::RwLock;

use h2_mvstore::replication::{DefaultChangeCollector, ReplicationChange, ReplicationListener};
use h2_mvstore::{MVStore, StorageEngine};
use h2_sql::SQLEngine;
use h2_types::{CacheInvalidationEvent, FencingToken, H2Error, H2Result, LogOpType, LogRecord, Lsn};

use crate::Connection;

/// コンピュートノードの動作ロール
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComputeRole {
    Primary,
    ReadReplica,
}

/// コンピュートノード内インメモリバッファプール / LRU キャッシュ
#[derive(Default)]
pub struct CachePool {
    entries: RwLock<std::collections::HashMap<String, std::collections::HashMap<Vec<u8>, (Vec<u8>, Lsn)>>>,
}

impl CachePool {
    pub fn new() -> Self {
        Self {
            entries: RwLock::new(std::collections::HashMap::new()),
        }
    }

    pub fn get(&self, map_name: &str, key: &[u8]) -> Option<Vec<u8>> {
        let guard = self.entries.read();
        guard
            .get(map_name)
            .and_then(|map| map.get(key))
            .map(|(val, _)| val.clone())
    }

    pub fn insert(&self, map_name: &str, key: Vec<u8>, val: Vec<u8>, lsn: Lsn) {
        let mut guard = self.entries.write();
        let map = guard.entry(map_name.to_string()).or_default();
        map.insert(key, (val, lsn));
    }

    pub fn invalidate(&self, map_name: &str, key: Option<&[u8]>) {
        let mut guard = self.entries.write();
        if let Some(map) = guard.get_mut(map_name) {
            if let Some(k) = key {
                map.remove(k);
            } else {
                map.clear();
            }
        }
    }

    pub fn count_cached_keys(&self, map_name: &str) -> usize {
        let guard = self.entries.read();
        guard.get(map_name).map(|m| m.len()).unwrap_or(0)
    }
}

/// Primary コンピュートノードのコミット時に、変更を WAL ログレコードに変換して
/// 分散ストレージフリートへクォーラム書き込みを行うコミットリスナー
struct ComputeQuorumCommitListener {
    storage_engine: Arc<dyn StorageEngine>,
    fencing_token: Arc<AtomicU64>,
    current_lsn: Arc<AtomicU64>,
    replica_listeners: Arc<RwLock<Vec<Arc<DecoupledComputeNode>>>>,
}

impl ReplicationListener for ComputeQuorumCommitListener {
    fn on_commit(&self, commit_version: u64, changes: Vec<ReplicationChange>) -> H2Result<()> {
        let token = FencingToken(self.fencing_token.load(Ordering::SeqCst));
        let base_lsn = self.current_lsn.load(Ordering::SeqCst);
        let mut cur_lsn = base_lsn;
        let mut records = Vec::with_capacity(changes.len() + 1);

        for change in &changes {
            cur_lsn += 1;
            let rec = if change.is_clear {
                LogRecord {
                    lsn: cur_lsn,
                    prev_lsn: cur_lsn - 1,
                    tx_id: commit_version,
                    op_type: LogOpType::TruncateMap,
                    map_name: change.map_name.clone(),
                    key: Vec::new(),
                    value: None,
                    timestamp_ms: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis() as u64,
                }
            } else if let Some(ref val) = change.value {
                LogRecord::put(cur_lsn, cur_lsn - 1, commit_version, &change.map_name, change.key.clone(), val.clone())
            } else {
                LogRecord::delete(cur_lsn, cur_lsn - 1, commit_version, &change.map_name, change.key.clone())
            };
            records.push(rec);
        }

        // コミットマーカーレコード
        cur_lsn += 1;
        records.push(LogRecord::commit(cur_lsn, cur_lsn - 1, commit_version));

        // 分散ストレージフリートへのクォーラム並行書き込み ("The Log is the Database")
        let acked_lsn = self.storage_engine.append_logs(&records, token)?;
        self.current_lsn.store(acked_lsn, Ordering::SeqCst);

        // 共有ストレージ・リードレプリカ群への軽量通知
        let replicas = self.replica_listeners.read().clone();
        for replica in replicas {
            replica.apply_remote_commit(commit_version, &changes, acked_lsn);
        }

        Ok(())
    }
}

/// コンピュート・ストレージ分離アーキテクチャのステートレスコンピュートノード
pub struct DecoupledComputeNode {
    pub node_id: String,
    role: RwLock<ComputeRole>,
    storage_engine: Arc<dyn StorageEngine>,
    store: Arc<MVStore>,
    engine: Arc<SQLEngine>,
    cache: Arc<CachePool>,
    current_lsn: Arc<AtomicU64>,
    fencing_token: Arc<AtomicU64>,
    replica_listeners: Arc<RwLock<Vec<Arc<DecoupledComputeNode>>>>,
}

impl DecoupledComputeNode {
    /// 新規 Primary コンピュートノードの作成
    pub fn new_primary(
        node_id: impl Into<String>,
        storage_engine: Arc<dyn StorageEngine>,
        fencing_token: FencingToken,
    ) -> Arc<Self> {
        let node_id_str = node_id.into();
        let store = Arc::new(MVStore::open_in_memory());
        let engine = Arc::new(SQLEngine::new(Arc::clone(&store)).expect("SQLEngine initialization failed"));
        engine.set_read_only(false);

        let current_lsn = Arc::new(AtomicU64::new(storage_engine.latest_committed_lsn()));
        let fencing_token_atomic = Arc::new(AtomicU64::new(fencing_token.0));
        let replica_listeners = Arc::new(RwLock::new(Vec::new()));

        // Primary 側のコミットシンクとリスナーを登録
        let collector = Arc::new(DefaultChangeCollector::new());
        store.set_change_sink(Some(collector));

        let listener = Arc::new(ComputeQuorumCommitListener {
            storage_engine: Arc::clone(&storage_engine),
            fencing_token: Arc::clone(&fencing_token_atomic),
            current_lsn: Arc::clone(&current_lsn),
            replica_listeners: Arc::clone(&replica_listeners),
        });
        store.set_replication_listener(Some(listener));

        Arc::new(Self {
            node_id: node_id_str,
            role: RwLock::new(ComputeRole::Primary),
            storage_engine,
            store,
            engine,
            cache: Arc::new(CachePool::new()),
            current_lsn,
            fencing_token: fencing_token_atomic,
            replica_listeners,
        })
    }

    /// 新規 Read Replica コンピュートノードの作成 (ゼロストレージ)
    pub fn new_read_replica(
        node_id: impl Into<String>,
        storage_engine: Arc<dyn StorageEngine>,
    ) -> Arc<Self> {
        let node_id_str = node_id.into();
        let store = Arc::new(MVStore::open_in_memory());
        let engine = Arc::new(SQLEngine::new(Arc::clone(&store)).expect("SQLEngine initialization failed"));
        engine.set_read_only(true); // 読み取り専用に設定

        let initial_lsn = storage_engine.latest_committed_lsn();

        Arc::new(Self {
            node_id: node_id_str,
            role: RwLock::new(ComputeRole::ReadReplica),
            storage_engine,
            store,
            engine,
            cache: Arc::new(CachePool::new()),
            current_lsn: Arc::new(AtomicU64::new(initial_lsn)),
            fencing_token: Arc::new(AtomicU64::new(0)),
            replica_listeners: Arc::new(RwLock::new(Vec::new())),
        })
    }

    pub fn role(&self) -> ComputeRole {
        *self.role.read()
    }

    pub fn is_primary(&self) -> bool {
        self.role() == ComputeRole::Primary
    }

    pub fn current_lsn(&self) -> Lsn {
        self.current_lsn.load(Ordering::SeqCst)
    }

    pub fn fencing_token(&self) -> FencingToken {
        FencingToken(self.fencing_token.load(Ordering::SeqCst))
    }

    pub fn cache(&self) -> &Arc<CachePool> {
        &self.cache
    }

    pub fn store(&self) -> &Arc<MVStore> {
        &self.store
    }

    pub fn engine(&self) -> &Arc<SQLEngine> {
        &self.engine
    }

    /// このコンピュートノード上で直接クエリ・トランザクションを実行できる Connection を生成
    pub fn connection(&self) -> Connection {
        Connection::from_engine(Arc::clone(&self.engine), Arc::clone(&self.store))
    }

    /// リードレプリカを登録（Primary からのキャッシュ無効化イベント通知先）
    pub fn register_replica(&self, replica: Arc<DecoupledComputeNode>) {
        self.replica_listeners.write().push(replica);
    }

    /// Primary からのコミット変更をローカルバッファキャッシュに反映
    pub fn apply_remote_commit(&self, commit_version: u64, changes: &[ReplicationChange], acked_lsn: Lsn) {
        let mut catalog_updated = false;
        for change in changes {
            if change.map_name == "_catalog" {
                catalog_updated = true;
            }
            let map = self.store.open_map(&change.map_name);
            if change.is_clear {
                map.clear();
                self.cache.invalidate(&change.map_name, None);
            } else if let Some(ref val) = change.value {
                map.put(change.key.clone(), val.clone());
                self.cache.insert(&change.map_name, change.key.clone(), val.clone(), acked_lsn);
            } else {
                map.remove(&change.key);
                self.cache.invalidate(&change.map_name, Some(&change.key));
            }
        }

        self.store.set_version(commit_version);
        if catalog_updated {
            let _ = self.engine.catalog().reload();
        }
        self.current_lsn.store(acked_lsn, Ordering::SeqCst);
    }

    /// キャッシュ無効化イベントの受信ハンドラ
    pub fn handle_invalidation(&self, event: &CacheInvalidationEvent) {
        self.cache.invalidate(&event.map_name, event.key.as_deref());
        self.current_lsn.fetch_max(event.lsn, Ordering::SeqCst);
    }

    /// キー直接書き込み (Primary のみ)
    pub fn put(&self, map_name: &str, key: Vec<u8>, value: Vec<u8>) -> H2Result<Lsn> {
        if !self.is_primary() {
            return Err(H2Error::ReadOnly("Cannot write on Read Replica".to_string()));
        }

        let map = self.store.open_map(map_name);
        map.put(key, value);
        let ver = self.store.commit()?;
        Ok(ver)
    }

    /// キー直接削除 (Primary のみ)
    pub fn delete(&self, map_name: &str, key: Vec<u8>) -> H2Result<Lsn> {
        if !self.is_primary() {
            return Err(H2Error::ReadOnly("Cannot write on Read Replica".to_string()));
        }

        let map = self.store.open_map(map_name);
        map.remove(&key);
        let ver = self.store.commit()?;
        Ok(ver)
    }

    /// キー直接取得 (Primary & Read Replica 共通)
    pub fn get(&self, map_name: &str, key: &[u8]) -> H2Result<Option<Vec<u8>>> {
        let map = self.store.open_map(map_name);
        if let Some(val) = map.get(key) {
            return Ok(Some(val));
        }

        // キャッシュミス時は共有ストレージ層からオンデマンドフェッチ
        let read_version = self.current_lsn();
        let val_opt = self.storage_engine.get_key(map_name, key, read_version)?;
        if let Some(ref val) = val_opt {
            map.put(key.to_vec(), val.clone());
        }
        Ok(val_opt)
    }

    /// 瞬間的フェイルオーバー: Read Replica から Primary への昇格 (Promote to Primary)
    /// 新世代 Fencing Token でストレージ層の排他的リースを獲得し、Redo リカバリなしで即座に昇格完了
    pub fn promote_to_primary(&self) -> H2Result<FencingToken> {
        if self.is_primary() {
            return Ok(self.fencing_token());
        }

        let current_storage_token = self.storage_engine.fencing_token();
        let new_token = current_storage_token.next();

        // ストレージ層のクォーラムノードからリースを獲得
        self.storage_engine.acquire_lease(new_token)?;

        // ロールを Primary に変更 & Read-Only を解除
        *self.role.write() = ComputeRole::Primary;
        self.engine.set_read_only(false);
        self.fencing_token.store(new_token.0, Ordering::SeqCst);

        // 最新 LSN を同期
        let latest_lsn = self.storage_engine.latest_committed_lsn();
        self.current_lsn.store(latest_lsn, Ordering::SeqCst);

        // 新 Primary 用のコミットリスナーとシンクを設定
        let collector = Arc::new(DefaultChangeCollector::new());
        self.store.set_change_sink(Some(collector));

        let listener = Arc::new(ComputeQuorumCommitListener {
            storage_engine: Arc::clone(&self.storage_engine),
            fencing_token: Arc::clone(&self.fencing_token),
            current_lsn: Arc::clone(&self.current_lsn),
            replica_listeners: Arc::clone(&self.replica_listeners),
        });
        self.store.set_replication_listener(Some(listener));

        Ok(new_token)
    }
}
