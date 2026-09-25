use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use parking_lot::RwLock;
use h2_types::H2Result;

use crate::chunk::ChunkPayload;
use crate::file_store::FileStore;
use crate::map::MVMap;
use crate::page::Page;
use crate::tree::MVTree;

/// MVStore 本体
pub struct MVStore {
    file_store: Arc<RwLock<FileStore>>,
    maps: Arc<RwLock<HashMap<String, MVMap>>>,
    version: Arc<RwLock<u64>>,
    chunk_id_counter: Arc<RwLock<u32>>,
}

impl MVStore {
    pub fn open<P: AsRef<Path>>(path: P) -> H2Result<Self> {
        let mut file_store = FileStore::open(path)?;
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
                if let Ok(root_page) = serde_json::from_slice::<Page>(&entry.value) {
                    let tree = MVTree {
                        root: Arc::new(root_page),
                        max_entries_per_page: 32,
                        version: payload.meta.version,
                    };
                    maps.insert(map_name.clone(), MVMap::new(map_name, tree));
                }
            }
        }

        Ok(Self {
            file_store: Arc::new(RwLock::new(file_store)),
            maps: Arc::new(RwLock::new(maps)),
            version: Arc::new(RwLock::new(last_version)),
            chunk_id_counter: Arc::new(RwLock::new(1)),
        })
    }

    pub fn open_in_memory() -> Self {
        Self {
            file_store: Arc::new(RwLock::new(FileStore::open_in_memory())),
            maps: Arc::new(RwLock::new(HashMap::new())),
            version: Arc::new(RwLock::new(0)),
            chunk_id_counter: Arc::new(RwLock::new(1)),
        }
    }

    /// 名前付きマップを取得または新規作成
    pub fn open_map(&self, name: &str) -> MVMap {
        let mut maps = self.maps.write();
        if let Some(map) = maps.get(name) {
            map.clone()
        } else {
            let new_map = MVMap::new(name, MVTree::default());
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

    /// 現在の全マップの変更をファイルに追記コミット
    pub fn commit(&self) -> H2Result<u64> {
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
            let tree_guard = map.tree.read();
            let root_bytes = serde_json::to_vec(&*tree_guard.root)
                .map_err(|e| h2_types::H2Error::Serialization(e.to_string()))?;
            metadata_tree.put(name.as_bytes().to_vec(), root_bytes);
        }

        let payload = ChunkPayload::new(chunk_id, new_version, (*metadata_tree.root).clone());

        let mut fs = self.file_store.write();
        fs.append_and_commit(&payload)?;

        Ok(new_version)
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
                if let Ok(vv) = serde_json::from_slice::<crate::tx::versioned_value::VersionedValue>(&entry.value) {
                    // Vacuum時点（current_ver）で確定している最新のコミット値を抽出
                    // 未コミットの変更は含めず、削除済みの場合はツリーから完全に除去（Vacuum）
                    if let Some(visible_val) = vv.read_visible(u64::MAX, current_ver) {
                        let clean_vv = crate::tx::versioned_value::VersionedValue::new_committed(
                            visible_val.to_vec(),
                            current_ver,
                        );
                        let clean_bytes = serde_json::to_vec(&clean_vv)
                            .map_err(|e| h2_types::H2Error::Serialization(e.to_string()))?;
                        clean_tree.put(entry.key, clean_bytes);
                    }
                } else {
                    // 通常データ（メタデータなど）はそのまま保持
                    clean_tree.put(entry.key, entry.value);
                }
            }

            let root_bytes = serde_json::to_vec(&*clean_tree.root)
                .map_err(|e| h2_types::H2Error::Serialization(e.to_string()))?;
            metadata_tree.put(name.as_bytes().to_vec(), root_bytes);
        }

        let payload = ChunkPayload::new(1, current_ver, (*metadata_tree.root).clone());

        let mut fs = self.file_store.write();
        fs.compact_and_rewrite(&payload)?;

        // チャンクIDカウンタをリセット
        *self.chunk_id_counter.write() = 1;

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
