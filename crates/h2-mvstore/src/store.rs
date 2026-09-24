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

    /// ストレージのコンパクション（Vacuum）を実行し、古い死にチャンクを回収
    pub fn compact(&self) -> H2Result<()> {
        let current_ver = *self.version.read();

        // 生存マップの最新ルートページのみを抽出してメタデータツリーを作成
        let mut metadata_tree = MVTree::default();
        let maps = self.maps.read();
        for (name, map) in maps.iter() {
            let tree_guard = map.tree.read();
            let root_bytes = serde_json::to_vec(&*tree_guard.root)
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
}
