use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use parking_lot::RwLock;
use crate::page::Entry;
use crate::replication::MapChangeSink;
use crate::tree::MVTree;
use h2_types::H2Result;

/// 名前付きキーバリューストア（マップ）
#[derive(Clone)]
pub struct MVMap {
    pub name: String,
    pub tree: Arc<RwLock<MVTree>>,
    sink: Arc<RwLock<Option<Arc<dyn MapChangeSink>>>>,
    mod_count: Arc<AtomicU64>,
    cached_root: Arc<RwLock<Option<(u64, Vec<u8>)>>>,
}

impl MVMap {
    pub fn new(name: impl Into<String>, tree: MVTree) -> Self {
        Self {
            name: name.into(),
            tree: Arc::new(RwLock::new(tree)),
            sink: Arc::new(RwLock::new(None)),
            mod_count: Arc::new(AtomicU64::new(1)),
            cached_root: Arc::new(RwLock::new(None)),
        }
    }

    pub fn set_sink(&self, sink: Option<Arc<dyn MapChangeSink>>) {
        *self.sink.write() = sink;
    }

    pub fn get(&self, key: &[u8]) -> Option<Vec<u8>> {
        self.tree.read().get(key)
    }

    pub fn put(&self, key: Vec<u8>, value: Vec<u8>) {
        if let Some(ref sink) = *self.sink.read() {
            sink.record_put(&self.name, key.clone(), value.clone());
        }
        self.mod_count.fetch_add(1, Ordering::Relaxed);
        self.tree.write().put(key, value);
    }

    pub fn remove(&self, key: &[u8]) -> Option<Vec<u8>> {
        if let Some(ref sink) = *self.sink.read() {
            sink.record_remove(&self.name, key.to_vec());
        }
        self.mod_count.fetch_add(1, Ordering::Relaxed);
        self.tree.write().remove(key)
    }

    pub fn scan_all(&self) -> Vec<Entry> {
        self.tree.read().scan_all()
    }

    pub fn scan_prefix(&self, prefix: &[u8]) -> Vec<Entry> {
        self.tree.read().scan_prefix(prefix)
    }

    pub fn scan_range(&self, start: std::ops::Bound<&[u8]>, end: std::ops::Bound<&[u8]>) -> Vec<Entry> {
        self.tree.read().scan_range(start, end)
    }

    /// マップ内の全エントリを一括消去 (O(1))
    pub fn clear(&self) {
        if let Some(ref sink) = *self.sink.read() {
            sink.record_clear(&self.name);
        }
        self.mod_count.fetch_add(1, Ordering::Relaxed);
        *self.tree.write() = MVTree::default();
    }

    /// シリアライズされたルートページのバイナリを取得（変更がなければキャッシュを再利用）
    pub fn get_serialized_root(&self) -> H2Result<Vec<u8>> {
        let current_mod = self.mod_count.load(Ordering::Relaxed);
        if let Some((mod_cnt, ref bytes)) = *self.cached_root.read() {
            if mod_cnt == current_mod {
                return Ok(bytes.clone());
            }
        }
        let tree_guard = self.tree.read();
        let bytes = bincode::serialize(&*tree_guard.root)
            .map_err(|e| h2_types::H2Error::Serialization(e.to_string()))?;
        *self.cached_root.write() = Some((current_mod, bytes.clone()));
        Ok(bytes)
    }
}
