pub mod chunk;
pub mod file_store;
pub mod map;
pub mod page;
pub mod store;
pub mod tree;

pub use chunk::{ChunkMeta, ChunkPayload};
pub use file_store::FileStore;
pub use map::MVMap;
pub use page::{Entry, Page, PageRef};
pub use store::MVStore;
pub use tree::MVTree;

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    #[test]
    fn test_in_memory_tree_crud() {
        let mut tree = MVTree::new(4); // 4エントリで分割テスト

        for i in 0..100 {
            let key = format!("key_{:04}", i).into_bytes();
            let val = format!("val_{:04}", i).into_bytes();
            tree.put(key, val);
        }

        for i in 0..100 {
            let key = format!("key_{:04}", i).into_bytes();
            let expected_val = format!("val_{:04}", i).into_bytes();
            assert_eq!(tree.get(&key), Some(expected_val));
        }

        assert_eq!(tree.get(b"non_existent"), None);

        // 削除テスト
        let removed = tree.remove(b"key_0050");
        assert_eq!(removed, Some(b"val_0050".to_vec()));
        assert_eq!(tree.get(b"key_0050"), None);

        // 全件走査テスト
        let entries = tree.scan_all();
        assert_eq!(entries.len(), 99);
    }

    #[test]
    fn test_mvstore_persistence_and_recovery() {
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path().to_path_buf();

        // 1. 初回起動と書き込み
        {
            let store = MVStore::open(&path).unwrap();
            let users_map = store.open_map("users");
            users_map.put(b"user:1".to_vec(), b"Alice".to_vec());
            users_map.put(b"user:2".to_vec(), b"Bob".to_vec());

            let config_map = store.open_map("config");
            config_map.put(b"cluster_size".to_vec(), b"3".to_vec());

            store.commit().unwrap();
        }

        // 2. 再起動（クラッシュリカバリ相当）
        {
            let store = MVStore::open(&path).unwrap();
            let users_map = store.open_map("users");
            assert_eq!(users_map.get(b"user:1"), Some(b"Alice".to_vec()));
            assert_eq!(users_map.get(b"user:2"), Some(b"Bob".to_vec()));

            let config_map = store.open_map("config");
            assert_eq!(config_map.get(b"cluster_size"), Some(b"3".to_vec()));

            // 追加書き込み
            users_map.put(b"user:3".to_vec(), b"Charlie".to_vec());
            store.commit().unwrap();
        }

        // 3. 3度目の起動
        {
            let store = MVStore::open(&path).unwrap();
            let users_map = store.open_map("users");
            assert_eq!(users_map.get(b"user:3"), Some(b"Charlie".to_vec()));
            assert_eq!(users_map.scan_all().len(), 3);
        }
    }
}
