use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use parking_lot::RwLock;
use h2_types::H2Result;

use crate::chunk::ChunkPayload;
use crate::file_store::FileStore;
use crate::map::MVMap;
use crate::page::Page;
use crate::replication::{MapChangeSink, ReplicationListener};
use crate::tree::MVTree;

/// MVStore 本体
pub struct MVStore {
    file_store: Arc<RwLock<FileStore>>,
    wal_manager: Arc<RwLock<crate::wal::WalManager>>,
    maps: Arc<RwLock<HashMap<String, MVMap>>>,
    version: Arc<RwLock<u64>>,
    chunk_id_counter: Arc<RwLock<u32>>,
    change_sink: Arc<RwLock<Option<Arc<dyn MapChangeSink>>>>,
    replication_listener: Arc<RwLock<Option<Arc<dyn ReplicationListener>>>>,
}

impl MVStore {
    pub fn open<P: AsRef<Path>>(path: P) -> H2Result<Self> {
        let path_ref = path.as_ref();
        let mut file_store = FileStore::open(path_ref)?;
        let last_version = file_store.header().version;

        let mut maps = HashMap::new();

        // 過去のチャンクからメタデータとマップツリーを復元
        if let Some(payload) = file_store.read_last_chunk()? {
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
            maps: Arc::new(RwLock::new(maps)),
            version: Arc::new(RwLock::new(max_version)),
            chunk_id_counter: Arc::new(RwLock::new(1)),
            change_sink: Arc::new(RwLock::new(None)),
            replication_listener: Arc::new(RwLock::new(None)),
        })
    }

    pub fn open_in_memory() -> Self {
        Self {
            file_store: Arc::new(RwLock::new(FileStore::open_in_memory())),
            wal_manager: Arc::new(RwLock::new(crate::wal::WalManager::open_in_memory())),
            maps: Arc::new(RwLock::new(HashMap::new())),
            version: Arc::new(RwLock::new(0)),
            chunk_id_counter: Arc::new(RwLock::new(1)),
            change_sink: Arc::new(RwLock::new(None)),
            replication_listener: Arc::new(RwLock::new(None)),
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
            Err(h2_types::H2Error::Storage(format!("Map '{}' not found", old_name)))
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

    /// トランザクション単位の超高速 WAL コミット（数十バイト追記）
    pub fn commit_wal(
        &self,
        tx_id: u64,
        commit_version: u64,
        changes: Vec<crate::wal::WalChange>,
    ) -> H2Result<()> {
        let mut ver_guard = self.version.write();
        if commit_version > *ver_guard {
            *ver_guard = commit_version;
        }

        let record = crate::wal::WalRecord {
            tx_id,
            commit_version,
            changes,
        };

        self.wal_manager.write().append(&record)?;

        // 未コミット変更の取り出し
        if let Some(ref sink) = *self.change_sink.read() {
            let _ = sink.drain_changes();
        }

        // レプリケーションリスナー（分散ストレージクォーラム等）が存在する場合に通知
        let listener_opt = self.replication_listener.read().clone();
        if let Some(ref listener) = listener_opt {
            let mut repl_changes = Vec::with_capacity(record.changes.len());
            for c in &record.changes {
                if let Some(ref v) = c.value {
                    repl_changes.push(crate::replication::ReplicationChange::put(c.map_name.clone(), c.key.clone(), v.clone()));
                } else {
                    repl_changes.push(crate::replication::ReplicationChange::remove(c.map_name.clone(), c.key.clone()));
                }
            }
            listener.on_commit(commit_version, repl_changes)?;
        }

        Ok(())
    }

    /// チェックポイント: 全マップの最新ツリーをファイルに記録し、WAL をクリア
    pub fn checkpoint(&self) -> H2Result<u64> {
        let mut ver_guard = self.version.write();
        *ver_guard += 1;
        let new_version = *ver_guard;

        let mut chunk_id_guard = self.chunk_id_counter.write();
        *chunk_id_guard += 1;
        let chunk_id = *chunk_id_guard;

        // 全マップのルートページをメタデータツリーに記録
        let mut metadata_tree = MVTree::default();
        let maps = self.maps.read();
        for (name, map) in maps.iter() {
            let root_bytes = map.get_serialized_root()?;
            metadata_tree.put(name.as_bytes().to_vec(), root_bytes);
        }

        let payload = ChunkPayload::new(chunk_id, new_version, (*metadata_tree.root).clone());

        // 未コミット変更の取り出し
        let changes = if let Some(ref sink) = *self.change_sink.read() {
            sink.drain_changes()
        } else {
            Vec::new()
        };

        let mut fs = self.file_store.write();
        fs.append_and_commit(&payload)?;

        // WAL の切り捨て
        let _ = self.wal_manager.write().clear();

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

    /// 全マップの全エントリをスキャンして取得（初期スナップショット送信用）
    pub fn scan_all_maps(&self) -> HashMap<String, Vec<(Vec<u8>, Vec<u8>)>> {
        let maps = self.maps.read();
        let mut result = HashMap::new();
        for (name, map) in maps.iter() {
            let entries = map.scan_all().into_iter().map(|e| (e.key, e.value)).collect();
            result.insert(name.clone(), entries);
        }
        result
    }

    /// スナップショットを全マップに適用（初期スナップショット受信用）
    pub fn apply_snapshot(&self, target_version: u64, snapshot: HashMap<String, Vec<(Vec<u8>, Vec<u8>)>>) -> H2Result<()> {
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
        *self.version.write() = ver;
    }

    pub fn current_version(&self) -> u64 {
        *self.version.read()
    }

    /// ストレージのオンラインコンパクション（Concurrent Vacuum）を実行
    /// 並行トランザクションをブロックせず、未コミット変更を排除し、削除済みレコードを完全回収して安全にファイルを圧縮置換
    pub fn compact(&self) -> H2Result<()> {
        let current_ver = *self.version.read();

        // 実行中のマップのスナップショットを取得（長時間ロックを避けるためマップ一覧をクローン）
        let map_clones: Vec<(String, MVMap)> = {
            let maps = self.maps.read();
            maps.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
        };

        // 各マップについて、コミット済みデータのみを抽出したクリーンツリーを構築
        let mut metadata_tree = MVTree::default();
        for (name, map) in map_clones {
            let raw_entries = map.scan_all();
            let mut clean_tree = MVTree::default();

            for entry in raw_entries {
                // VersionedValue（MVCCレコード）の場合
                if let Ok(vv) = crate::tx::versioned_value::VersionedValue::from_bytes(&entry.value) {
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

        let mut fs = self.file_store.write();
        fs.compact_and_rewrite(&payload)?;

        // チャンクIDカウンタをリセット
        *self.chunk_id_counter.write() = 1;

        let _ = self.wal_manager.write().clear();

        Ok(())
    }

    pub fn get_map_names(&self) -> Vec<String> {
        self.maps.read().keys().cloned().collect()
    }

    /// 全マップのデータをファイルへバックアップ
    pub fn dump_backup<P: AsRef<Path>>(&self, path: P) -> H2Result<()> {
        let maps = self.maps.read();
        let mut backup_data: HashMap<String, Vec<(Vec<u8>, Vec<u8>)>> = HashMap::new();
        for (name, map) in maps.iter() {
            let entries = map.scan_all().into_iter().map(|e| (e.key, e.value)).collect();
            backup_data.insert(name.clone(), entries);
        }
        let serialized = serde_json::to_vec_pretty(&backup_data)
            .map_err(|e| h2_types::H2Error::Serialization(e.to_string()))?;
        std::fs::write(path, serialized)
            .map_err(|e| h2_types::H2Error::Storage(e.to_string()))?;
        Ok(())
    }

    /// バックアップファイルから全マップを復元
    pub fn restore_backup<P: AsRef<Path>>(&self, path: P) -> H2Result<()> {
        let data = std::fs::read(path)
            .map_err(|e| h2_types::H2Error::Storage(e.to_string()))?;
        let backup_data: HashMap<String, Vec<(Vec<u8>, Vec<u8>)>> = serde_json::from_slice(&data)
            .map_err(|e| h2_types::H2Error::Serialization(e.to_string()))?;

        for (name, entries) in backup_data {
            let map = self.open_map(&name);
            map.clear();
            for (k, v) in entries {
                map.put(k, v);
            }
        }
        self.commit()?;
        Ok(())
    }
}
