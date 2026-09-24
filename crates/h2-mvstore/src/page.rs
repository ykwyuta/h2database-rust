use std::sync::Arc;
use serde::{Deserialize, Serialize};

/// ページ参照（永続化ファイル内のチャンクIDとチャンク内オフセット）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PageRef {
    pub chunk_id: u32,
    pub page_offset: u32,
}

impl PageRef {
    pub const MEMORY_ONLY: Self = Self {
        chunk_id: 0,
        page_offset: 0,
    };
}

/// B-Treeのキーと値のエントリ
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub key: Vec<u8>,
    pub value: Vec<u8>,
}

/// B-Treeのページ（ノード）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Page {
    Leaf {
        entries: Vec<Entry>,
    },
    Branch {
        keys: Vec<Vec<u8>>,
        children: Vec<Arc<Page>>,
        child_refs: Vec<PageRef>,
    },
}

impl Page {
    pub fn new_leaf() -> Self {
        Page::Leaf {
            entries: Vec::new(),
        }
    }

    pub fn new_branch(keys: Vec<Vec<u8>>, children: Vec<Arc<Page>>) -> Self {
        let child_refs = vec![PageRef::MEMORY_ONLY; children.len()];
        Page::Branch {
            keys,
            children,
            child_refs,
        }
    }

    pub fn is_leaf(&self) -> bool {
        matches!(self, Page::Leaf { .. })
    }

    pub fn entry_count(&self) -> usize {
        match self {
            Page::Leaf { entries } => entries.len(),
            Page::Branch { keys, .. } => keys.len(),
        }
    }

    /// キーを二分探索してインデックスを返す
    pub fn find_key(&self, target_key: &[u8]) -> Result<usize, usize> {
        match self {
            Page::Leaf { entries } => {
                entries.binary_search_by(|e| e.key.as_slice().cmp(target_key))
            }
            Page::Branch { keys, .. } => {
                keys.binary_search_by(|k| k.as_slice().cmp(target_key))
            }
        }
    }

    /// 葉ページから値を取得
    pub fn get(&self, key: &[u8]) -> Option<&[u8]> {
        match self {
            Page::Leaf { entries } => {
                if let Ok(idx) = self.find_key(key) {
                    Some(&entries[idx].value)
                } else {
                    None
                }
            }
            Page::Branch { .. } => None,
        }
    }
}
