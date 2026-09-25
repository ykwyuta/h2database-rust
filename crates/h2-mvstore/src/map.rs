use crate::page::Entry;
use crate::replication::MapChangeSink;
use crate::tree::MVTree;
use h2_types::H2Result;
use parking_lot::{Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

#[derive(Default)]
pub(crate) struct MapJournal {
    enabled: bool,
    pub clear: bool,
    pub changes: HashMap<Vec<u8>, Option<Vec<u8>>>,
}

impl MapJournal {
    pub(crate) fn reset(&mut self) {
        self.clear = false;
        self.changes.clear();
    }
}

/// 名前付きキーバリューストア（マップ）
#[derive(Clone)]
pub struct MVMap {
    pub name: String,
    pub tree: Arc<RwLock<MVTree>>,
    sink: Arc<RwLock<Option<Arc<dyn MapChangeSink>>>>,
    mod_count: Arc<AtomicU64>,
    cached_root: Arc<RwLock<Option<(u64, Vec<u8>)>>>,
    journal: Arc<Mutex<MapJournal>>,
}

impl MVMap {
    fn read_tree(&self) -> RwLockReadGuard<'_, MVTree> {
        if h2_types::query_metrics::enabled() {
            if let Some(guard) = self.tree.try_read() {
                return guard;
            }
            let start = Instant::now();
            let guard = self.tree.read();
            h2_types::query_metrics::record_tree_lock_wait(start.elapsed());
            guard
        } else {
            self.tree.read()
        }
    }

    fn write_tree(&self) -> RwLockWriteGuard<'_, MVTree> {
        if h2_types::query_metrics::enabled() {
            if let Some(guard) = self.tree.try_write() {
                return guard;
            }
            let start = Instant::now();
            let guard = self.tree.write();
            h2_types::query_metrics::record_tree_lock_wait(start.elapsed());
            guard
        } else {
            self.tree.write()
        }
    }

    pub fn new(name: impl Into<String>, tree: MVTree) -> Self {
        Self {
            name: name.into(),
            tree: Arc::new(RwLock::new(tree)),
            sink: Arc::new(RwLock::new(None)),
            mod_count: Arc::new(AtomicU64::new(1)),
            cached_root: Arc::new(RwLock::new(None)),
            journal: Arc::new(Mutex::new(MapJournal::default())),
        }
    }

    pub fn set_sink(&self, sink: Option<Arc<dyn MapChangeSink>>) {
        *self.sink.write() = sink;
    }

    pub fn get(&self, key: &[u8]) -> Option<Vec<u8>> {
        self.read_tree().get(key)
    }

    pub fn put(&self, key: Vec<u8>, value: Vec<u8>) {
        if let Some(ref sink) = *self.sink.read() {
            sink.record_put(&self.name, key.clone(), value.clone());
        }
        let mut journal = self.journal.lock();
        self.mod_count.fetch_add(1, Ordering::Relaxed);
        self.write_tree().put(key.clone(), value.clone());
        if journal.enabled {
            journal.changes.insert(key, Some(value));
        }
    }

    pub fn remove(&self, key: &[u8]) -> Option<Vec<u8>> {
        if let Some(ref sink) = *self.sink.read() {
            sink.record_remove(&self.name, key.to_vec());
        }
        let mut journal = self.journal.lock();
        self.mod_count.fetch_add(1, Ordering::Relaxed);
        let removed = self.write_tree().remove(key);
        if removed.is_some() && journal.enabled {
            journal.changes.insert(key.to_vec(), None);
        }
        removed
    }

    pub fn scan_all(&self) -> Vec<Entry> {
        self.read_tree().scan_all()
    }

    pub fn for_each_entry<F: FnMut(&[u8], &[u8])>(&self, f: F) {
        self.read_tree().for_each_entry(f)
    }

    pub fn scan_prefix(&self, prefix: &[u8]) -> Vec<Entry> {
        self.read_tree().scan_prefix(prefix)
    }

    pub fn scan_range(
        &self,
        start: std::ops::Bound<&[u8]>,
        end: std::ops::Bound<&[u8]>,
    ) -> Vec<Entry> {
        self.read_tree().scan_range(start, end)
    }

    /// マップ内の全エントリを一括消去 (O(1))
    pub fn clear(&self) {
        if let Some(ref sink) = *self.sink.read() {
            sink.record_clear(&self.name);
        }
        let mut journal = self.journal.lock();
        self.mod_count.fetch_add(1, Ordering::Relaxed);
        *self.write_tree() = MVTree::default();
        if journal.enabled {
            journal.clear = true;
            journal.changes.clear();
        }
    }

    /// シリアライズされたルートページのバイナリを取得（変更がなければキャッシュを再利用）
    pub fn get_serialized_root(&self) -> H2Result<Vec<u8>> {
        let current_mod = self.mod_count.load(Ordering::Relaxed);
        if let Some((mod_cnt, ref bytes)) = *self.cached_root.read() {
            if mod_cnt == current_mod {
                return Ok(bytes.clone());
            }
        }
        let tree_guard = self.read_tree();
        let bytes = bincode::serialize(&*tree_guard.root)
            .map_err(|e| h2_types::H2Error::Serialization(e.to_string()))?;
        *self.cached_root.write() = Some((current_mod, bytes.clone()));
        Ok(bytes)
    }

    pub fn modification_count(&self) -> u64 {
        self.mod_count.load(Ordering::Relaxed)
    }

    pub(crate) fn take_journal(&self) -> MapJournal {
        let mut journal = self.journal.lock();
        let enabled = journal.enabled;
        std::mem::replace(
            &mut *journal,
            MapJournal {
                enabled,
                ..MapJournal::default()
            },
        )
    }

    pub(crate) fn set_journaling(&self, enabled: bool) {
        self.journal.lock().enabled = enabled;
    }

    pub(crate) fn freeze_mutations(&self) -> MutexGuard<'_, MapJournal> {
        self.journal.lock()
    }

    pub(crate) fn restore_journal(&self, old: MapJournal) {
        let mut current = self.journal.lock();
        if current.clear {
            return;
        }
        if old.clear {
            let recent = std::mem::take(&mut current.changes);
            current.clear = true;
            current.changes = old.changes;
            current.changes.extend(recent);
        } else {
            for (key, value) in old.changes {
                current.changes.entry(key).or_insert(value);
            }
        }
    }
}
