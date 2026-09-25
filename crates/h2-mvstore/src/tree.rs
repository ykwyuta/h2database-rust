use std::ops::Bound;
use std::sync::Arc;
use crate::page::{Entry, Page};

pub const DEFAULT_PAGE_SPLIT_SIZE: usize = 32;

/// Copy-on-Write B-Tree
#[derive(Debug, Clone)]
pub struct MVTree {
    pub root: Arc<Page>,
    pub max_entries_per_page: usize,
    pub version: u64,
}

impl Default for MVTree {
    fn default() -> Self {
        Self::new(DEFAULT_PAGE_SPLIT_SIZE)
    }
}

impl MVTree {
    pub fn new(max_entries_per_page: usize) -> Self {
        Self {
            root: Arc::new(Page::new_leaf()),
            max_entries_per_page,
            version: 0,
        }
    }

    pub fn get(&self, key: &[u8]) -> Option<Vec<u8>> {
        let mut curr = Arc::clone(&self.root);
        loop {
            match curr.as_ref() {
                Page::Leaf { entries } => {
                    return match entries.binary_search_by(|e| e.key.as_slice().cmp(key)) {
                        Ok(idx) => Some(entries[idx].value.clone()),
                        Err(_) => None,
                    };
                }
                Page::Branch { keys, children, .. } => {
                    let idx = match keys.binary_search_by(|k| k.as_slice().cmp(key)) {
                        Ok(i) => i + 1,
                        Err(i) => i,
                    };
                    curr = Arc::clone(&children[idx]);
                }
            }
        }
    }

    /// キーバリューの挿入または更新（CoWにより新しいルートを生成）
    pub fn put(&mut self, key: Vec<u8>, value: Vec<u8>) {
        let (new_page, split) = self.insert_recursive(Arc::clone(&self.root), key, value);
        if let Some((pivot, right)) = split {
            // ルートが分割されたため、新たなルート枝を作成
            let new_root = Page::new_branch(vec![pivot], vec![new_page, right]);
            self.root = Arc::new(new_root);
        } else {
            self.root = new_page;
        }
    }

    /// キーの削除
    pub fn remove(&mut self, key: &[u8]) -> Option<Vec<u8>> {
        let (new_page, old_val) = self.remove_recursive(Arc::clone(&self.root), key);
        if let Some(val) = old_val {
            self.root = new_page;
            Some(val)
        } else {
            None
        }
    }

    fn insert_recursive(
        &self,
        node: Arc<Page>,
        key: Vec<u8>,
        value: Vec<u8>,
    ) -> (Arc<Page>, Option<(Vec<u8>, Arc<Page>)>) {
        match node.as_ref() {
            Page::Leaf { entries } => {
                let mut new_entries = entries.clone();
                match new_entries.binary_search_by(|e| e.key.as_slice().cmp(&key)) {
                    Ok(idx) => {
                        // 既存キーの更新
                        new_entries[idx].value = value;
                        (Arc::new(Page::Leaf { entries: new_entries }), None)
                    }
                    Err(idx) => {
                        // 新規キーの挿入
                        new_entries.insert(idx, Entry { key, value });
                        if new_entries.len() > self.max_entries_per_page {
                            // 分割（Split）
                            let mid = new_entries.len() / 2;
                            let right_entries = new_entries.split_off(mid);
                            let pivot = right_entries[0].key.clone();

                            let left_page = Arc::new(Page::Leaf { entries: new_entries });
                            let right_page = Arc::new(Page::Leaf {
                                entries: right_entries,
                            });
                            (left_page, Some((pivot, right_page)))
                        } else {
                            (Arc::new(Page::Leaf { entries: new_entries }), None)
                        }
                    }
                }
            }
            Page::Branch {
                keys,
                children,
                child_refs: _,
            } => {
                let idx = match keys.binary_search_by(|k| k.as_slice().cmp(&key)) {
                    Ok(i) => i + 1,
                    Err(i) => i,
                };

                let target_child = Arc::clone(&children[idx]);
                let (new_child, split) = self.insert_recursive(target_child, key, value);

                let mut new_keys = keys.clone();
                let mut new_children = children.clone();
                new_children[idx] = new_child;

                if let Some((pivot, right_child)) = split {
                    new_keys.insert(idx, pivot);
                    new_children.insert(idx + 1, right_child);

                    if new_keys.len() > self.max_entries_per_page {
                        // 枝ノードの分割
                        let mid = new_keys.len() / 2;
                        let pivot = new_keys.remove(mid);
                        let right_keys = new_keys.split_off(mid);
                        let right_children = new_children.split_off(mid + 1);

                        let left_branch = Arc::new(Page::new_branch(new_keys, new_children));
                        let right_branch =
                            Arc::new(Page::new_branch(right_keys, right_children));
                        (left_branch, Some((pivot, right_branch)))
                    } else {
                        (
                            Arc::new(Page::new_branch(new_keys, new_children)),
                            None,
                        )
                    }
                } else {
                    (
                        Arc::new(Page::new_branch(new_keys, new_children)),
                        None,
                    )
                }
            }
        }
    }

    fn remove_recursive(&self, node: Arc<Page>, key: &[u8]) -> (Arc<Page>, Option<Vec<u8>>) {
        match node.as_ref() {
            Page::Leaf { entries } => {
                let mut new_entries = entries.clone();
                match new_entries.binary_search_by(|e| e.key.as_slice().cmp(key)) {
                    Ok(idx) => {
                        let removed = new_entries.remove(idx);
                        (
                            Arc::new(Page::Leaf { entries: new_entries }),
                            Some(removed.value),
                        )
                    }
                    Err(_) => (node, None),
                }
            }
            Page::Branch {
                keys,
                children,
                child_refs: _,
            } => {
                let idx = match keys.binary_search_by(|k| k.as_slice().cmp(key)) {
                    Ok(i) => i + 1,
                    Err(i) => i,
                };
                let target_child = Arc::clone(&children[idx]);
                let (new_child, old_val) = self.remove_recursive(target_child, key);
                if old_val.is_some() {
                    let mut new_children = children.clone();
                    new_children[idx] = new_child;
                    (
                        Arc::new(Page::new_branch(keys.clone(), new_children)),
                        old_val,
                    )
                } else {
                    (node, None)
                }
            }
        }
    }

    /// 全エントリの走査
    pub fn scan_all(&self) -> Vec<Entry> {
        let mut results = Vec::new();
        self.collect_entries(&self.root, &mut results);
        results
    }

    fn collect_entries(&self, node: &Page, out: &mut Vec<Entry>) {
        match node {
            Page::Leaf { entries } => {
                out.extend(entries.iter().cloned());
            }
            Page::Branch { children, .. } => {
                for child in children {
                    self.collect_entries(child, out);
                }
            }
        }
    }

    /// 指定されたプレフィックスで始まる全エントリの高速走査（B+Tree の二分探索）
    pub fn scan_prefix(&self, prefix: &[u8]) -> Vec<Entry> {
        let mut results = Vec::new();
        self.collect_prefix_entries(&self.root, prefix, &mut results);
        results
    }

    fn collect_prefix_entries(&self, node: &Page, prefix: &[u8], out: &mut Vec<Entry>) {
        match node {
            Page::Leaf { entries } => {
                let start_idx = match entries.binary_search_by(|e| e.key.as_slice().cmp(prefix)) {
                    Ok(idx) => idx,
                    Err(idx) => idx,
                };
                for entry in &entries[start_idx..] {
                    if entry.key.starts_with(prefix) {
                        out.push(entry.clone());
                    } else if entry.key.as_slice() > prefix {
                        break;
                    }
                }
            }
            Page::Branch { keys, children, .. } => {
                for (i, child) in children.iter().enumerate() {
                    if i > 0 {
                        let l = &keys[i - 1];
                        if prefix < l.as_slice() && !l.starts_with(prefix) {
                            continue;
                        }
                    }
                    if i < keys.len() {
                        let r = &keys[i];
                        if prefix >= r.as_slice() && !r.starts_with(prefix) {
                            continue;
                        }
                    }
                    self.collect_prefix_entries(child, prefix, out);
                }
            }
        }
    }

    /// 指定された範囲 [start, end] の全エントリの高速走査（真の B+Tree Range Scan）
    pub fn scan_range(&self, start: Bound<&[u8]>, end: Bound<&[u8]>) -> Vec<Entry> {
        let mut results = Vec::new();
        self.collect_range_entries(&self.root, start, end, &mut results);
        results
    }

    fn collect_range_entries(
        &self,
        node: &Page,
        start: Bound<&[u8]>,
        end: Bound<&[u8]>,
        out: &mut Vec<Entry>,
    ) {
        match node {
            Page::Leaf { entries } => {
                let start_idx = match start {
                    Bound::Unbounded => 0,
                    Bound::Included(k) => match entries.binary_search_by(|e| e.key.as_slice().cmp(k)) {
                        Ok(idx) | Err(idx) => idx,
                    },
                    Bound::Excluded(k) => match entries.binary_search_by(|e| e.key.as_slice().cmp(k)) {
                        Ok(idx) => idx + 1,
                        Err(idx) => idx,
                    },
                };
                for entry in &entries[start_idx..] {
                    let k = entry.key.as_slice();
                    let within_end = match end {
                        Bound::Unbounded => true,
                        Bound::Included(end_k) => k <= end_k,
                        Bound::Excluded(end_k) => k < end_k,
                    };
                    if within_end {
                        out.push(entry.clone());
                    } else {
                        break;
                    }
                }
            }
            Page::Branch { keys, children, .. } => {
                for (i, child) in children.iter().enumerate() {
                    if i > 0 {
                        let lower = keys[i - 1].as_slice();
                        match end {
                            Bound::Included(end_k) if end_k < lower => continue,
                            Bound::Excluded(end_k) if end_k <= lower => continue,
                            _ => {}
                        }
                    }
                    if i < keys.len() {
                        let upper = keys[i].as_slice();
                        match start {
                            Bound::Included(start_k) if start_k >= upper => continue,
                            Bound::Excluded(start_k) if start_k >= upper => continue,
                            _ => {}
                        }
                    }
                    self.collect_range_entries(child, start, end, out);
                }
            }
        }
    }
}
