use h2_types::{H2Error, H2Result};
use parking_lot::{Condvar, Mutex, MutexGuard, RwLock};
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::chunk::ChunkPayload;
use crate::delta::{DeltaPayload, MapDelta};
use crate::file_store::FileStore;
use crate::map::{MVMap, MapJournal};
use crate::page::Page;
use crate::replication::{MapChangeSink, ReplicationListener};
use crate::tree::MVTree;

#[derive(Default)]
struct CommitQueue {
    pending: Vec<PendingCommit>,
    leader: bool,
    batch_start_offset: Option<u64>,
    poisoned: Option<String>,
}

struct PendingCommit {
    record: crate::wal::WalRecord,
    completed: Arc<(Mutex<Option<Result<(), (String, bool)>>>, Condvar)>,
}

struct ActiveCommit<'a>(&'a AtomicUsize);

pub(crate) struct CommitFailure {
    pub error: H2Error,
    pub durable: bool,
}

impl From<H2Error> for CommitFailure {
    fn from(error: H2Error) -> Self {
        Self {
            error,
            durable: false,
        }
    }
}

impl Drop for ActiveCommit<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

/// MVStore 本体
pub struct MVStore {
    file_store: Arc<RwLock<FileStore>>,
    wal_manager: Arc<RwLock<crate::wal::WalManager>>,
    wal_archiver: Arc<RwLock<Option<crate::wal::WalArchiver>>>,
    maps: Arc<RwLock<HashMap<String, MVMap>>>,
    version: Arc<RwLock<u64>>,
    chunk_id_counter: Arc<RwLock<u32>>,
    change_sink: Arc<RwLock<Option<Arc<dyn MapChangeSink>>>>,
    replication_listener: Arc<RwLock<Option<Arc<dyn ReplicationListener>>>>,
    last_checkpoint: Mutex<Instant>,
    checkpointed_maps: Mutex<HashMap<String, u64>>,
    has_checkpoint: AtomicBool,
    delta_depth: AtomicU32,
    checkpoint_gate: RwLock<()>,
    commit_queue: Mutex<CommitQueue>,
    next_commit_version: AtomicU64,
    active_committers: AtomicUsize,
}

impl MVStore {
    fn lock_commit_queue(&self) -> MutexGuard<'_, CommitQueue> {
        if h2_types::query_metrics::enabled() {
            if let Some(guard) = self.commit_queue.try_lock() {
                return guard;
            }
            let started = Instant::now();
            let guard = self.commit_queue.lock();
            h2_types::query_metrics::record_commit_lock_wait(started.elapsed());
            guard
        } else {
            self.commit_queue.lock()
        }
    }

    fn enter_commit_gate(&self) -> parking_lot::RwLockReadGuard<'_, ()> {
        if h2_types::query_metrics::enabled() {
            if let Some(guard) = self.checkpoint_gate.try_read() {
                return guard;
            }
            let started = Instant::now();
            let guard = self.checkpoint_gate.read();
            h2_types::query_metrics::record_commit_lock_wait(started.elapsed());
            guard
        } else {
            self.checkpoint_gate.read()
        }
    }

    pub fn open<P: AsRef<Path>>(path: P) -> H2Result<Self> {
        let path_ref = path.as_ref();
        let mut file_store = FileStore::open(path_ref)?;
        let last_version = file_store.header().version;

        let mut maps = HashMap::new();

        // 全量チャンクを基点に差分チェックポイントを順番に適用する。
        let (base, deltas) = file_store.read_checkpoint_chain()?;
        let has_checkpoint = base.is_some() || !deltas.is_empty();
        let delta_depth = deltas.len() as u32;
        if let Some(payload) = base {
            let metadata_tree = MVTree {
                root: Arc::new(payload.root_page),
                max_entries_per_page: 32,
                version: payload.meta.version,
            };

            // メタデータマップから登録済みマップのルート情報を復元
            for entry in metadata_tree.scan_all() {
                let map_name = String::from_utf8_lossy(&entry.key).to_string();
                let root_page_opt = bincode::deserialize::<Page>(&entry.value).ok();
                if let Some(root_page) = root_page_opt {
                    let tree = MVTree {
                        root: Arc::new(root_page),
                        max_entries_per_page: 32,
                        version: payload.meta.version,
                    };
                    maps.insert(map_name.clone(), MVMap::new(map_name, tree));
                }
            }
        }

        for delta in deltas {
            for name in delta.removed_maps {
                maps.remove(&name);
            }
            for change in delta.maps {
                let map = if let Some(root_bytes) = change.replace_root {
                    let root: Page = bincode::deserialize(&root_bytes)
                        .map_err(|e| h2_types::H2Error::Serialization(e.to_string()))?;
                    let tree = MVTree {
                        root: Arc::new(root),
                        max_entries_per_page: 32,
                        version: delta.version,
                    };
                    let map = MVMap::new(&change.name, tree);
                    maps.insert(change.name.clone(), map.clone());
                    map
                } else {
                    maps.get(&change.name).cloned().ok_or_else(|| {
                        h2_types::H2Error::Corrupted(format!(
                            "Delta references missing map: {}",
                            change.name
                        ))
                    })?
                };
                if change.clear {
                    map.clear();
                }
                for (key, value) in change.changes {
                    if let Some(value) = value {
                        map.put(key, value);
                    } else {
                        map.remove(&key);
                    }
                }
            }
        }

        // WAL から復元した変更も次の差分チェックポイントに含める。
        let checkpointed_maps: HashMap<String, u64> = maps
            .iter()
            .map(|(name, map)| (name.clone(), map.modification_count()))
            .collect();
        for map in maps.values() {
            map.set_journaling(true);
        }

        // WAL の復元と差分適用（クラッシュリカバリ）
        let sync_on_commit = file_store.sync_on_commit();
        let mut wal_manager = crate::wal::WalManager::open(path_ref, sync_on_commit)?;
        let wal_records = wal_manager.read_all_records()?;
        let mut max_version = last_version;
        for record in wal_records {
            if record.commit_version > max_version {
                max_version = record.commit_version;
            }
            for change in record.changes {
                let map = if let Some(m) = maps.get(&change.map_name) {
                    m.clone()
                } else {
                    let m = MVMap::new(&change.map_name, MVTree::default());
                    m.set_journaling(true);
                    maps.insert(change.map_name.clone(), m.clone());
                    m
                };
                if let Some(val) = change.value {
                    map.put(change.key, val);
                } else {
                    map.remove(&change.key);
                }
            }
        }

        Ok(Self {
            file_store: Arc::new(RwLock::new(file_store)),
            wal_manager: Arc::new(RwLock::new(wal_manager)),
            wal_archiver: Arc::new(RwLock::new(None)),
            maps: Arc::new(RwLock::new(maps)),
            version: Arc::new(RwLock::new(max_version)),
            chunk_id_counter: Arc::new(RwLock::new(1)),
            change_sink: Arc::new(RwLock::new(None)),
            replication_listener: Arc::new(RwLock::new(None)),
            last_checkpoint: Mutex::new(Instant::now()),
            checkpointed_maps: Mutex::new(checkpointed_maps),
            has_checkpoint: AtomicBool::new(has_checkpoint),
            delta_depth: AtomicU32::new(delta_depth),
            checkpoint_gate: RwLock::new(()),
            commit_queue: Mutex::new(CommitQueue::default()),
            next_commit_version: AtomicU64::new(max_version),
            active_committers: AtomicUsize::new(0),
        })
    }

    pub fn open_in_memory() -> Self {
        Self {
            file_store: Arc::new(RwLock::new(FileStore::open_in_memory())),
            wal_manager: Arc::new(RwLock::new(crate::wal::WalManager::open_in_memory())),
            wal_archiver: Arc::new(RwLock::new(None)),
            maps: Arc::new(RwLock::new(HashMap::new())),
            version: Arc::new(RwLock::new(0)),
            chunk_id_counter: Arc::new(RwLock::new(1)),
            change_sink: Arc::new(RwLock::new(None)),
            replication_listener: Arc::new(RwLock::new(None)),
            last_checkpoint: Mutex::new(Instant::now()),
            checkpointed_maps: Mutex::new(HashMap::new()),
            has_checkpoint: AtomicBool::new(false),
            delta_depth: AtomicU32::new(0),
            checkpoint_gate: RwLock::new(()),
            commit_queue: Mutex::new(CommitQueue::default()),
            next_commit_version: AtomicU64::new(0),
            active_committers: AtomicUsize::new(0),
        }
    }

    /// マップ変更シンクを設定
    pub fn set_change_sink(&self, sink: Option<Arc<dyn MapChangeSink>>) {
        let maps = self.maps.read();
        for map in maps.values() {
            map.set_sink(sink.as_ref().map(Arc::clone));
        }
        *self.change_sink.write() = sink;
    }

    /// レプリケーションリスナーを設定
    pub fn set_replication_listener(&self, listener: Option<Arc<dyn ReplicationListener>>) {
        *self.replication_listener.write() = listener;
    }

    /// 名前付きマップを取得または新規作成
    pub fn open_map(&self, name: &str) -> MVMap {
        let mut maps = self.maps.write();
        if let Some(map) = maps.get(name) {
            map.clone()
        } else {
            let new_map = MVMap::new(name, MVTree::default());
            new_map.set_journaling(self.has_checkpoint.load(Ordering::Acquire));
            if let Some(ref sink) = *self.change_sink.read() {
                new_map.set_sink(Some(Arc::clone(sink)));
            }
            maps.insert(name.to_string(), new_map.clone());
            new_map
        }
    }

    /// 名前付きマップをリネーム (O(1))
    pub fn rename_map(&self, old_name: &str, new_name: &str) -> H2Result<()> {
        let mut maps = self.maps.write();
        if let Some(mut map) = maps.remove(old_name) {
            map.name = new_name.to_string();
            maps.insert(new_name.to_string(), map);
            Ok(())
        } else {
            Err(h2_types::H2Error::Storage(format!(
                "Map '{}' not found",
                old_name
            )))
        }
    }

    /// 名前付きマップの全データを一括消去 (O(1))
    pub fn clear_map(&self, name: &str) {
        let maps = self.maps.read();
        if let Some(map) = maps.get(name) {
            map.clear();
        }
    }

    /// 名前付きマップを削除
    pub fn remove_map(&self, name: &str) -> bool {
        self.maps.write().remove(name).is_some()
    }

    /// 未コミット値から WAL レコードを準備し、複数レコードを一度の同期で確定する。
    /// `prepare` はツリーを変更せず、指定された版の確定値を返す。
    pub(crate) fn commit_with<F>(&self, tx_id: u64, prepare: F) -> Result<(), CommitFailure>
    where
        F: FnOnce(u64) -> H2Result<Vec<crate::wal::WalChange>>,
    {
        let _checkpoint_guard = self.enter_commit_gate();
        self.active_committers.fetch_add(1, Ordering::Relaxed);
        let _active = ActiveCommit(&self.active_committers);
        let completed = Arc::new((Mutex::new(None), Condvar::new()));
        let mut queue = self.lock_commit_queue();
        if let Some(error) = &queue.poisoned {
            return Err(H2Error::Storage(error.clone()).into());
        }

        let commit_version = self.next_commit_version.fetch_add(1, Ordering::Relaxed) + 1;
        let changes = prepare(commit_version)?;
        let timestamp_nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as i64;
        let record = crate::wal::WalRecord {
            tx_id,
            commit_version,
            changes,
            timestamp_nanos,
        };
        {
            let mut wal = self.wal_manager.write();
            let before = wal.len()?;
            if queue.pending.is_empty() {
                queue.batch_start_offset = Some(before);
            }
            if let Err(error) = wal.append_unsynced(&record) {
                if let Err(rollback_error) = wal.truncate_to(before) {
                    queue.poisoned = Some(format!(
                        "WAL append failed and rollback failed: {rollback_error}"
                    ));
                }
                return Err(error.into());
            }
        }
        queue.pending.push(PendingCommit {
            record,
            completed: Arc::clone(&completed),
        });
        let is_leader = !queue.leader;
        queue.leader = true;
        drop(queue);

        let wait_started = Instant::now();
        if is_leader {
            if self.active_committers.load(Ordering::Relaxed) > 1
                && self.wal_manager.read().sync_on_commit()
            {
                let spin_start = Instant::now();
                while spin_start.elapsed() < Duration::from_micros(50) {
                    std::hint::spin_loop();
                    if self.active_committers.load(Ordering::Relaxed) <= 1 {
                        break;
                    }
                }
            }
            let mut queue = self.lock_commit_queue();
            let pending = std::mem::take(&mut queue.pending);
            let batch_start = queue.batch_start_offset.take().unwrap_or(0);
            let sync_started = Instant::now();
            let result = self
                .wal_manager
                .write()
                .sync_for_commit()
                .map_err(|error| (error.to_string(), false));
            h2_types::query_metrics::record_wal_sync(sync_started.elapsed());
            let mut outcomes = vec![result.clone(); pending.len()];
            if result.is_ok() {
                for item in &pending {
                    for change in &item.record.changes {
                        let map = self.open_map(&change.map_name);
                        if let Some(value) = &change.value {
                            map.put(change.key.clone(), value.clone());
                        } else {
                            map.remove(&change.key);
                        }
                    }
                }
                if let Some(last) = pending.last() {
                    *self.version.write() = last.record.commit_version;
                }
                if let Some(ref mut archiver) = *self.wal_archiver.write() {
                    for item in &pending {
                        let _ = archiver.archive_record(&item.record);
                    }
                }
                if let Some(ref sink) = *self.change_sink.read() {
                    let _ = sink.drain_changes();
                }
                if let Some(listener) = self.replication_listener.read().clone() {
                    for (index, item) in pending.iter().enumerate() {
                        let repl_changes = item
                            .record
                            .changes
                            .iter()
                            .map(|change| match &change.value {
                                Some(value) => crate::replication::ReplicationChange::put(
                                    change.map_name.clone(),
                                    change.key.clone(),
                                    value.clone(),
                                ),
                                None => crate::replication::ReplicationChange::remove(
                                    change.map_name.clone(),
                                    change.key.clone(),
                                ),
                            })
                            .collect();
                        if let Err(error) =
                            listener.on_commit(item.record.commit_version, repl_changes)
                        {
                            outcomes[index] = Err((error.to_string(), true));
                        }
                    }
                }
            } else {
                let mut wal = self.wal_manager.write();
                if let Err(error) = wal.truncate_to(batch_start).and_then(|_| wal.sync()) {
                    queue.poisoned = Some(format!("WAL sync failed and rollback failed: {error}"));
                }
            }
            for (item, outcome) in pending.into_iter().zip(outcomes) {
                let (lock, cv) = &*item.completed;
                *lock.lock() = Some(outcome);
                cv.notify_one();
            }
            queue.leader = false;
        }

        let (lock, cv) = &*completed;
        let mut result = lock.lock();
        while result.is_none() {
            cv.wait(&mut result);
        }
        h2_types::query_metrics::record_wal_durable_wait(wait_started.elapsed());
        result
            .take()
            .unwrap()
            .map_err(|(message, durable)| CommitFailure {
                error: H2Error::Storage(message),
                durable,
            })
    }

    /// 初回は全量、以降は変更キーのみを記録し、WAL をクリアする。
    pub fn checkpoint(&self) -> H2Result<u64> {
        let _checkpoint_guard = self.checkpoint_gate.write();
        let mut ver_guard = self.version.write();
        let new_version = *ver_guard + 1;

        let mut chunk_id_guard = self.chunk_id_counter.write();
        *chunk_id_guard += 1;
        let chunk_id = *chunk_id_guard;

        let maps = self.maps.read();
        let mut checkpointed_maps = HashMap::with_capacity(maps.len());
        let first_checkpoint = !self.has_checkpoint.load(Ordering::Acquire);
        let previous_maps = self.checkpointed_maps.lock().clone();
        let mut drained: Vec<(MVMap, MapJournal)> = Vec::with_capacity(maps.len());
        let mut map_deltas = Vec::new();
        let mut metadata_tree = MVTree::default();
        for (name, map) in maps.iter() {
            if first_checkpoint {
                map.set_journaling(true);
            }
            let modification_count = map.modification_count();
            let journal = map.take_journal();
            if first_checkpoint {
                match map.get_serialized_root() {
                    Ok(root_bytes) => metadata_tree.put(name.as_bytes().to_vec(), root_bytes),
                    Err(error) => {
                        map.restore_journal(journal);
                        for (map, journal) in drained {
                            map.restore_journal(journal);
                        }
                        return Err(error);
                    }
                }
            } else if !previous_maps.contains_key(name) {
                let root_bytes = match map.get_serialized_root() {
                    Ok(bytes) => bytes,
                    Err(error) => {
                        map.restore_journal(journal);
                        for (map, journal) in drained {
                            map.restore_journal(journal);
                        }
                        return Err(error);
                    }
                };
                map_deltas.push(MapDelta {
                    name: name.clone(),
                    replace_root: Some(root_bytes),
                    clear: false,
                    changes: Vec::new(),
                });
            } else if journal.clear || !journal.changes.is_empty() {
                map_deltas.push(MapDelta {
                    name: name.clone(),
                    replace_root: None,
                    clear: journal.clear,
                    changes: journal
                        .changes
                        .iter()
                        .map(|(key, value)| (key.clone(), value.clone()))
                        .collect(),
                });
            }
            checkpointed_maps.insert(name.clone(), modification_count);
            drained.push((map.clone(), journal));
        }

        // 未コミット変更の取り出し
        let changes = if let Some(ref sink) = *self.change_sink.read() {
            sink.drain_changes()
        } else {
            Vec::new()
        };

        let mut fs = self.file_store.write();
        let write_result = if first_checkpoint {
            let payload = ChunkPayload::new(chunk_id, new_version, (*metadata_tree.root).clone());
            fs.append_and_commit(&payload)
        } else {
            let removed_maps = previous_maps
                .keys()
                .filter(|name| !maps.contains_key(*name))
                .cloned()
                .collect();
            fs.append_delta_and_commit(DeltaPayload {
                version: new_version,
                previous_offset: 0,
                previous_length: 0,
                removed_maps,
                maps: map_deltas,
            })
        };
        if let Err(error) = write_result {
            for (map, journal) in drained {
                map.restore_journal(journal);
            }
            return Err(error);
        }

        *ver_guard = new_version;
        self.next_commit_version
            .fetch_max(new_version, Ordering::Relaxed);
        self.has_checkpoint.store(true, Ordering::Release);
        if !first_checkpoint {
            self.delta_depth.fetch_add(1, Ordering::Relaxed);
        }
        *self.checkpointed_maps.lock() = checkpointed_maps;
        *self.last_checkpoint.lock() = Instant::now();

        // WAL の切り捨て
        self.wal_manager.write().clear()?;

        // レプリケーションリスナーが存在する場合、通知＆同期待機 (remote_apply)
        if let Some(ref listener) = *self.replication_listener.read() {
            listener.on_commit(new_version, changes)?;
        }

        Ok(new_version)
    }

    /// 現在の全マップの変更をファイルに追記コミット（チェックポイント）
    pub fn commit(&self) -> H2Result<u64> {
        self.checkpoint()
    }

    pub fn set_sync_on_commit(&self, sync: bool) {
        self.file_store.write().set_sync_on_commit(sync);
        self.wal_manager.write().set_sync_on_commit(sync);
    }

    pub fn sync(&self) -> H2Result<()> {
        self.checkpoint()?;
        self.file_store.write().sync()?;
        self.wal_manager.write().sync()?;
        Ok(())
    }

    /// WAL だけを永続化する。全マップのスナップショットは作らない。
    pub fn sync_wal(&self) -> H2Result<()> {
        self.wal_manager.write().sync()
    }

    /// WAL の量または経過時間に応じてチェックポイントを作る。
    /// 変更がなければ、経過時間だけを理由に全量を書き出さない。
    pub fn checkpoint_if_needed(
        &self,
        max_wal_bytes: u64,
        max_interval: Duration,
    ) -> H2Result<bool> {
        let wal_bytes = self.wal_manager.read().len()?;
        let elapsed = self.last_checkpoint.lock().elapsed();
        let changed = {
            let maps = self.maps.read();
            let checkpointed = self.checkpointed_maps.lock();
            maps.len() != checkpointed.len()
                || maps
                    .iter()
                    .any(|(name, map)| checkpointed.get(name) != Some(&map.modification_count()))
        };
        if !changed && wal_bytes == 0 {
            return Ok(false);
        }
        if wal_bytes < max_wal_bytes && elapsed < max_interval {
            return Ok(false);
        }
        self.checkpoint()?;
        if self.delta_depth.load(Ordering::Relaxed) >= 64 {
            self.reclaim_checkpoint_history()?;
        }
        Ok(true)
    }

    /// 差分チェーンを現在の全量スナップショットにまとめ、古いチャンクを回収する。
    pub fn reclaim_checkpoint_history(&self) -> H2Result<u64> {
        let _checkpoint_guard = self.checkpoint_gate.write();
        let mut version = self.version.write();
        let maps = self.maps.read();
        // put/remove は journal を先に取得するため、全マップの変更を停止して
        // スナップショットと WAL 切り詰めの境界を固定する。
        let mut frozen: Vec<_> = maps.values().map(MVMap::freeze_mutations).collect();
        let mut metadata_tree = MVTree::default();
        let mut checkpointed_maps = HashMap::with_capacity(maps.len());
        for (name, map) in maps.iter() {
            metadata_tree.put(name.as_bytes().to_vec(), map.get_serialized_root()?);
            checkpointed_maps.insert(name.clone(), map.modification_count());
        }
        let new_version = *version + 1;
        let payload = ChunkPayload::new(1, new_version, (*metadata_tree.root).clone());
        self.file_store.write().compact_and_rewrite(&payload)?;
        *version = new_version;
        self.next_commit_version
            .fetch_max(new_version, Ordering::Relaxed);
        self.wal_manager.write().clear()?;
        for journal in &mut frozen {
            journal.reset();
        }
        *self.checkpointed_maps.lock() = checkpointed_maps;
        *self.last_checkpoint.lock() = Instant::now();
        self.delta_depth.store(0, Ordering::Relaxed);
        *self.chunk_id_counter.write() = 1;
        self.has_checkpoint.store(true, Ordering::Release);
        Ok(new_version)
    }

    /// 全マップの全エントリをスキャンして取得（初期スナップショット送信用）
    pub fn scan_all_maps(&self) -> HashMap<String, Vec<(Vec<u8>, Vec<u8>)>> {
        let maps = self.maps.read();
        let mut result = HashMap::new();
        for (name, map) in maps.iter() {
            let entries = map
                .scan_all()
                .into_iter()
                .map(|e| (e.key, e.value))
                .collect();
            result.insert(name.clone(), entries);
        }
        result
    }

    /// スナップショットを全マップに適用（初期スナップショット受信用）
    pub fn apply_snapshot(
        &self,
        target_version: u64,
        snapshot: HashMap<String, Vec<(Vec<u8>, Vec<u8>)>>,
    ) -> H2Result<()> {
        for (name, entries) in snapshot {
            let map = self.open_map(&name);
            map.clear();
            for (k, v) in entries {
                map.put(k, v);
            }
        }
        *self.version.write() = target_version.saturating_sub(1);
        self.commit()?;
        *self.version.write() = target_version;
        Ok(())
    }

    pub fn set_version(&self, ver: u64) {
        let _checkpoint_guard = self.checkpoint_gate.write();
        *self.version.write() = ver;
        self.next_commit_version.fetch_max(ver, Ordering::Relaxed);
    }

    pub fn current_version(&self) -> u64 {
        *self.version.read()
    }

    /// 未コミット変更を除いてファイルを再構築する。置換中は書き込みを停止する。
    pub fn compact(&self) -> H2Result<()> {
        let _checkpoint_guard = self.checkpoint_gate.write();
        let version = self.version.write();
        let current_ver = *version;
        let maps = self.maps.read();
        let mut frozen: Vec<_> = maps.values().map(MVMap::freeze_mutations).collect();

        // 各マップについて、コミット済みデータのみを抽出したクリーンツリーを構築
        let mut metadata_tree = MVTree::default();
        for (name, map) in maps.iter() {
            let raw_entries = map.scan_all();
            let mut clean_tree = MVTree::default();

            for entry in raw_entries {
                // VersionedValue（MVCCレコード）の場合
                if let Ok(vv) = crate::tx::versioned_value::VersionedValue::from_bytes(&entry.value)
                {
                    // Vacuum時点（current_ver）で確定している最新のコミット値を抽出
                    // 未コミットの変更は含めず、削除済みの場合はツリーから完全に除去（Vacuum）
                    if let Some(visible_val) = vv.read_visible(u64::MAX, current_ver) {
                        let clean_vv = crate::tx::versioned_value::VersionedValue::new_committed(
                            visible_val.to_vec(),
                            current_ver,
                        );
                        let clean_bytes = clean_vv.to_bytes()?;
                        clean_tree.put(entry.key, clean_bytes);
                    }
                } else {
                    // 通常データ（メタデータなど）はそのまま保持
                    clean_tree.put(entry.key, entry.value);
                }
            }

            let root_bytes = bincode::serialize(&*clean_tree.root)
                .map_err(|e| h2_types::H2Error::Serialization(e.to_string()))?;
            metadata_tree.put(name.as_bytes().to_vec(), root_bytes);
        }

        let payload = ChunkPayload::new(1, current_ver, (*metadata_tree.root).clone());

        self.file_store.write().compact_and_rewrite(&payload)?;

        // チャンクIDカウンタをリセット
        *self.chunk_id_counter.write() = 1;

        self.wal_manager.write().clear()?;
        for journal in &mut frozen {
            journal.reset();
        }
        *self.checkpointed_maps.lock() = maps
            .iter()
            .map(|(name, map)| (name.clone(), map.modification_count()))
            .collect();
        *self.last_checkpoint.lock() = Instant::now();
        self.delta_depth.store(0, Ordering::Relaxed);
        self.has_checkpoint.store(true, Ordering::Release);

        Ok(())
    }

    /// WAL アーカイバを有効化
    pub fn enable_wal_archiver<P: AsRef<Path>>(&self, archive_dir: P) -> H2Result<()> {
        let archiver = crate::wal::WalArchiver::new(archive_dir)?;
        *self.wal_archiver.write() = Some(archiver);
        Ok(())
    }

    /// WAL アーカイバを無効化
    pub fn disable_wal_archiver(&self) {
        *self.wal_archiver.write() = None;
    }

    /// WAL アーカイバへの参照を取得
    pub fn wal_archiver(&self) -> Arc<RwLock<Option<crate::wal::WalArchiver>>> {
        Arc::clone(&self.wal_archiver)
    }

    pub fn get_map_names(&self) -> Vec<String> {
        self.maps.read().keys().cloned().collect()
    }

    /// 全マップのデータを高速バイナリファイルへバックアップ
    pub fn dump_backup<P: AsRef<Path>>(&self, path: P) -> H2Result<crate::backup::BackupMetadata> {
        self.dump_backup_with_role(path, false)
    }

    /// ロール（Primary または Read Replica）を指定してバックアップを出力
    pub fn dump_backup_with_role<P: AsRef<Path>>(
        &self,
        path: P,
        is_replica: bool,
    ) -> H2Result<crate::backup::BackupMetadata> {
        let maps = self.maps.read();
        let mut backup_data: HashMap<String, Vec<(Vec<u8>, Vec<u8>)>> = HashMap::new();
        for (name, map) in maps.iter() {
            let entries = map
                .scan_all()
                .into_iter()
                .map(|e| (e.key, e.value))
                .collect();
            backup_data.insert(name.clone(), entries);
        }
        let current_ver = *self.version.read();
        let mut file = std::fs::File::create(path)?;
        crate::backup::write_binary_backup(&mut file, current_ver, is_replica, &backup_data)
    }

    /// バックアップファイルの整合性を検証（データ書き換えなし: VERIFYONLY 相当）
    pub fn verify_backup<P: AsRef<Path>>(&self, path: P) -> H2Result<crate::backup::BackupMetadata> {
        let mut file = std::fs::File::open(path)?;
        crate::backup::verify_binary_backup(&mut file)
    }

    /// 高速バイナリバックアップファイルから全マップを復元
    pub fn restore_backup<P: AsRef<Path>>(&self, path: P) -> H2Result<()> {
        let mut file = std::fs::File::open(path)?;
        let (meta, backup_data) = crate::backup::read_and_verify_binary_backup(&mut file)?;

        for (name, entries) in backup_data {
            let map = self.open_map(&name);
            map.clear();
            for (k, v) in entries {
                map.put(k, v);
            }
        }
        if meta.snapshot_version > 0 {
            self.set_version(meta.snapshot_version);
        }
        self.commit()?;
        Ok(())
    }

    /// ベースバックアップと継続 WAL アーカイブを組み合わせた任意時点復旧 (PITR)
    pub fn restore_pitr<P: AsRef<Path>, A: AsRef<Path>>(
        &self,
        backup_path: P,
        archive_dir: Option<A>,
        target: &crate::wal::RecoveryTarget,
    ) -> H2Result<crate::wal::RestoreReport> {
        let mut file = std::fs::File::open(backup_path)?;
        let (meta, backup_data) = crate::backup::read_and_verify_binary_backup(&mut file)?;
        let base_snapshot_version = meta.snapshot_version;
        let is_replica_backup = meta.is_replica;

        // 1. ベースバックアップの復元
        for (name, entries) in backup_data {
            let map = self.open_map(&name);
            map.clear();
            for (k, v) in entries {
                map.put(k, v);
            }
        }
        self.set_version(base_snapshot_version);

        let mut final_recovered_version = base_snapshot_version;
        let mut records_replayed = 0;
        let mut target_reached = true;

        // 2. WAL アーカイブからのロールフォワード
        if let Some(archive_path) = archive_dir {
            let records = crate::wal::WalArchiver::read_archive_records(archive_path, base_snapshot_version)?;
            for record in records {
                let should_apply = match target {
                    crate::wal::RecoveryTarget::Version(v) => record.commit_version <= *v,
                    crate::wal::RecoveryTarget::TimestampNanos(t) => record.timestamp_nanos <= *t,
                    crate::wal::RecoveryTarget::TransactionId(tx) => record.tx_id <= *tx,
                    crate::wal::RecoveryTarget::Latest => true,
                };

                if !should_apply {
                    target_reached = true;
                    break;
                }

                for change in record.changes {
                    let map = self.open_map(&change.map_name);
                    if let Some(val) = change.value {
                        map.put(change.key, val);
                    } else {
                        map.remove(&change.key);
                    }
                }

                final_recovered_version = record.commit_version;
                records_replayed += 1;
            }
        }

        self.set_version(final_recovered_version);
        self.commit()?;

        Ok(crate::wal::RestoreReport {
            base_snapshot_version,
            final_recovered_version,
            records_replayed,
            target_reached,
            is_replica_backup,
        })
    }
}
