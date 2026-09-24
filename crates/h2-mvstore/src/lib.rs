pub mod chunk;
pub mod file_store;
pub mod map;
pub mod page;
pub mod store;
pub mod tree;
pub mod tx;

pub use chunk::{ChunkMeta, ChunkPayload};
pub use file_store::FileStore;
pub use map::MVMap;
pub use page::{Entry, Page, PageRef};
pub use store::MVStore;
pub use tree::MVTree;
pub use tx::{Transaction, TransactionStatus, TransactionStore, VersionedValue};

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
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

    #[test]
    fn test_mvcc_transaction_isolation_and_rollback() {
        let store = Arc::new(MVStore::open_in_memory());
        let tx_store = TransactionStore::new(store);

        // 1. 初期コミット
        {
            let tx1 = tx_store.begin();
            tx1.put("accounts", b"alice".to_vec(), b"100".to_vec()).unwrap();
            tx1.put("accounts", b"bob".to_vec(), b"50".to_vec()).unwrap();
            tx1.commit().unwrap();
        }

        // 2. スナップショット分離（tx2実行中にtx3がコミットしても、tx2にはtx3の変更が見えない）
        let tx2 = tx_store.begin();
        {
            let tx3 = tx_store.begin();
            tx3.put("accounts", b"alice".to_vec(), b"150".to_vec()).unwrap();
            tx3.commit().unwrap();
        }

        // tx2 から見ると alice はまだ 100 のまま（スナップショット分離）
        assert_eq!(tx2.get("accounts", b"alice").unwrap(), Some(b"100".to_vec()));
        tx2.commit().unwrap();

        // 新規トランザクション tx4 からは 150 が見える
        let tx4 = tx_store.begin();
        assert_eq!(tx4.get("accounts", b"alice").unwrap(), Some(b"150".to_vec()));

        // 3. ロールバックの検証
        tx4.put("accounts", b"alice".to_vec(), b"999".to_vec()).unwrap();
        assert_eq!(tx4.get("accounts", b"alice").unwrap(), Some(b"999".to_vec()));
        tx4.rollback().unwrap();

        // ロールバック後は 150 のまま
        let tx5 = tx_store.begin();
        assert_eq!(tx5.get("accounts", b"alice").unwrap(), Some(b"150".to_vec()));
    }

    #[test]
    fn test_mvcc_write_conflict() {
        let store = Arc::new(MVStore::open_in_memory());
        let tx_store = TransactionStore::new(store);

        let tx1 = tx_store.begin();
        let tx2 = tx_store.begin();

        tx1.put("test", b"key1".to_vec(), b"val1".to_vec()).unwrap();

        // tx1が未コミットのままtx2が同じキーを変更しようとすると競合エラーになる
        let conflict_res = tx2.put("test", b"key1".to_vec(), b"val2".to_vec());
        assert!(conflict_res.is_err());

        // tx1 をコミット
        tx1.commit().unwrap();

        // その後の tx3 は変更可能
        let tx3 = tx_store.begin();
        tx3.put("test", b"key1".to_vec(), b"val3".to_vec()).unwrap();
        tx3.commit().unwrap();
    }
}
