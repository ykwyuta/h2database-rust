use std::sync::Arc;
use parking_lot::RwLock;
use crate::page::Entry;
use crate::tree::MVTree;

/// 名前付きキーバリューストア（マップ）
#[derive(Clone)]
pub struct MVMap {
    pub name: String,
    pub tree: Arc<RwLock<MVTree>>,
}

impl MVMap {
    pub fn new(name: impl Into<String>, tree: MVTree) -> Self {
        Self {
            name: name.into(),
            tree: Arc::new(RwLock::new(tree)),
        }
    }

    pub fn get(&self, key: &[u8]) -> Option<Vec<u8>> {
        self.tree.read().get(key)
    }

    pub fn put(&self, key: Vec<u8>, value: Vec<u8>) {
        self.tree.write().put(key, value);
    }

    pub fn remove(&self, key: &[u8]) -> Option<Vec<u8>> {
        self.tree.write().remove(key)
    }

    pub fn scan_all(&self) -> Vec<Entry> {
        self.tree.read().scan_all()
    }
}
