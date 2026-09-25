use std::sync::Arc;
use parking_lot::RwLock;
use crate::page::Entry;
use crate::replication::MapChangeSink;
use crate::tree::MVTree;

/// 名前付きキーバリューストア（マップ）
#[derive(Clone)]
pub struct MVMap {
    pub name: String,
    pub tree: Arc<RwLock<MVTree>>,
    sink: Arc<RwLock<Option<Arc<dyn MapChangeSink>>>>,
}

impl MVMap {
    pub fn new(name: impl Into<String>, tree: MVTree) -> Self {
        Self {
            name: name.into(),
            tree: Arc::new(RwLock::new(tree)),
            sink: Arc::new(RwLock::new(None)),
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
        self.tree.write().put(key, value);
    }

    pub fn remove(&self, key: &[u8]) -> Option<Vec<u8>> {
        if let Some(ref sink) = *self.sink.read() {
            sink.record_remove(&self.name, key.to_vec());
        }
        self.tree.write().remove(key)
    }

    pub fn scan_all(&self) -> Vec<Entry> {
        self.tree.read().scan_all()
    }

    /// マップ内の全エントリを一括消去 (O(1))
    pub fn clear(&self) {
        if let Some(ref sink) = *self.sink.read() {
            sink.record_clear(&self.name);
        }
        *self.tree.write() = MVTree::default();
    }
}
