pub mod backup;
pub mod buffer_pool;
pub mod chunk;
pub mod delta;
pub mod file_store;
pub mod map;
pub mod page;
pub mod replication;
pub mod storage_engine;
pub mod store;
pub mod tree;
pub mod tx;
pub mod wal;

pub use backup::{
    is_binary_backup_file, is_binary_backup_file_stream, read_and_verify_binary_backup,
    verify_binary_backup, write_binary_backup, BackupMetadata,
};
pub use buffer_pool::{BufferPoolManager, ClockReplacer, DiskManager, PageFrame, SlottedPage, PAGE_SIZE};
pub use chunk::{ChunkMeta, ChunkPayload};
pub use file_store::FileStore;
pub use map::MVMap;
pub use page::{Entry, Page, PageRef};
pub use replication::{DefaultChangeCollector, MapChangeSink, ReplicationChange, ReplicationCommitRecord, ReplicationListener};
pub use storage_engine::{LocalMVStoreEngine, StorageEngine};
pub use store::MVStore;
pub use tree::MVTree;
pub use tx::{Transaction, TransactionStatus, TransactionStore, VersionedValue};
pub use wal::{
    RecoveryTarget, RestoreReport, WalArchiveMeta, WalArchiver, WalChange, WalManager, WalRecord,
};

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

            // scan_prefix テスト
            let user_entries = users_map.scan_prefix(b"user:");
            assert_eq!(user_entries.len(), 3);
            let u1_entries = users_map.scan_prefix(b"user:1");
            assert_eq!(u1_entries.len(), 1);
            assert_eq!(u1_entries[0].value, b"Alice");
        }
    }

    #[test]
    fn test_query_metrics_capture_durable_wal() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(MVStore::open(dir.path().join("metrics.db")).unwrap());
        store.set_sync_on_commit(true);
        let tx_store = TransactionStore::new(store);
        let metrics = h2_types::query_metrics::QueryMetricsGuard::start();
        let tx = tx_store.begin();
        tx.put("accounts", b"alice".to_vec(), b"100".to_vec()).unwrap();
        tx.commit().unwrap();
        let counters = metrics.finish();
        assert!(counters.wal_write_ns > 0);
        assert!(counters.wal_sync_ns > 0);
    }

    #[test]
    fn checkpoint_policy_skips_unchanged_data_and_recovers_wal() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("checkpoint_policy.db");
        let store = Arc::new(MVStore::open(&path).unwrap());
        let tx_store = TransactionStore::new(Arc::clone(&store));
        let initial_size = std::fs::metadata(&path).unwrap().len();

        assert!(!store.checkpoint_if_needed(1, std::time::Duration::ZERO).unwrap());
        let tx = tx_store.begin();
        tx.put("accounts", b"alice".to_vec(), b"100".to_vec()).unwrap();
        tx.commit().unwrap();
        store.sync_wal().unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().len(), initial_size);

        drop(tx_store);
        drop(store);
        let store = Arc::new(MVStore::open(&path).unwrap());
        let tx_store = TransactionStore::new(Arc::clone(&store));
        let tx = tx_store.begin();
        assert_eq!(tx.get("accounts", b"alice").unwrap(), Some(b"100".to_vec()));
        tx.commit().unwrap();

        assert!(store.checkpoint_if_needed(1, std::time::Duration::from_secs(60)).unwrap());
        let checkpoint_size = std::fs::metadata(&path).unwrap().len();
        assert!(checkpoint_size > initial_size);
        assert!(!store.checkpoint_if_needed(1, std::time::Duration::ZERO).unwrap());
        assert_eq!(std::fs::metadata(&path).unwrap().len(), checkpoint_size);

        drop(tx_store);
        drop(store);
        let reopened = MVStore::open(&path).unwrap();
        assert!(reopened.open_map("accounts").get(b"alice").is_some());
    }

    #[test]
    fn delta_checkpoint_writes_only_changed_keys_and_reopens() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("delta.db");
        let store = MVStore::open(&path).unwrap();
        let map = store.open_map("items");
        for i in 0..1000u32 {
            map.put(i.to_be_bytes().to_vec(), vec![b'x'; 128]);
        }
        store.sync().unwrap();
        let full_size = std::fs::metadata(&path).unwrap().len();
        map.put(42u32.to_be_bytes().to_vec(), vec![b'y'; 128]);
        store.sync().unwrap();
        let delta_size = std::fs::metadata(&path).unwrap().len() - full_size;
        assert!(delta_size < full_size / 10, "delta={delta_size}, full={full_size}");
        drop(store);

        let store = MVStore::open(&path).unwrap();
        let map = store.open_map("items");
        assert_eq!(map.get(&42u32.to_be_bytes()), Some(vec![b'y'; 128]));
        assert_eq!(map.get(&43u32.to_be_bytes()), Some(vec![b'x'; 128]));
        map.remove(&43u32.to_be_bytes());
        store.sync().unwrap();
        drop(store);

        let store = MVStore::open(&path).unwrap();
        assert_eq!(store.open_map("items").get(&43u32.to_be_bytes()), None);
        assert_eq!(store.open_map("items").scan_all().len(), 999);
        let before_reclaim = std::fs::metadata(&path).unwrap().len();
        store.reclaim_checkpoint_history().unwrap();
        assert!(std::fs::metadata(&path).unwrap().len() < before_reclaim);
        drop(store);
        let store = MVStore::open(&path).unwrap();
        assert_eq!(store.open_map("items").get(&42u32.to_be_bytes()), Some(vec![b'y'; 128]));
        assert_eq!(store.open_map("items").get(&43u32.to_be_bytes()), None);
    }

    #[test]
    fn recovered_wal_changes_survive_following_delta_checkpoint() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wal_delta.db");
        {
            let store = Arc::new(MVStore::open(&path).unwrap());
            store.sync().unwrap();
            let tx_store = TransactionStore::new(store);
            let tx = tx_store.begin();
            tx.put("accounts", b"alice".to_vec(), b"100".to_vec()).unwrap();
            tx.commit().unwrap();
        }
        {
            let store = MVStore::open(&path).unwrap();
            assert!(store.checkpoint_if_needed(1, std::time::Duration::from_secs(60)).unwrap());
        }
        let store = Arc::new(MVStore::open(&path).unwrap());
        let tx_store = TransactionStore::new(store);
        let tx = tx_store.begin();
        assert_eq!(tx.get("accounts", b"alice").unwrap(), Some(b"100".to_vec()));
    }

    #[test]
    fn concurrent_distinct_commits_have_ordered_versions_and_recover() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("parallel.db");
        let store = Arc::new(MVStore::open(&path).unwrap());
        let tx_store = TransactionStore::new(Arc::clone(&store));
        let barrier = Arc::new(std::sync::Barrier::new(8));
        let handles: Vec<_> = (0..8u8).map(|worker| {
            let tx_store = Arc::clone(&tx_store);
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                for value in 0..50u8 {
                    let tx = tx_store.begin();
                    tx.put("parallel", vec![worker], vec![value]).unwrap();
                    tx.commit().unwrap();
                }
            })
        }).collect();
        for handle in handles {
            handle.join().unwrap();
        }
        assert_eq!(store.current_version(), 400);
        let mut wal = WalManager::open(&path, true).unwrap();
        let records = wal.read_all_records().unwrap();
        assert_eq!(records.len(), 400);
        assert!(records.windows(2).all(|pair| pair[0].commit_version < pair[1].commit_version));
        drop(wal);
        drop(tx_store);
        drop(store);

        let store = Arc::new(MVStore::open(&path).unwrap());
        let tx_store = TransactionStore::new(store);
        let tx = tx_store.begin();
        for worker in 0..8u8 {
            assert_eq!(tx.get("parallel", &[worker]).unwrap(), Some(vec![49]));
        }
    }

    #[test]
    fn wal_recovery_truncates_torn_tail_before_new_commits() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("torn.db");
        let make_record = |version| WalRecord {
            tx_id: version, commit_version: version,
            changes: vec![WalChange {
                map_name: "items".to_string(), key: vec![version as u8], value: Some(vec![version as u8]),
            }],
            timestamp_nanos: 0,
        };
        {
            let mut wal = WalManager::open(&path, true).unwrap();
            wal.append(&make_record(1)).unwrap();
        }
        let mut file = std::fs::OpenOptions::new().append(true)
            .open(path.with_extension("wal")).unwrap();
        file.write_all(&[0x11, 0x22, 0x33, 0x44]).unwrap();
        drop(file);
        {
            let mut wal = WalManager::open(&path, true).unwrap();
            assert_eq!(wal.read_all_records().unwrap().len(), 1);
            wal.append(&make_record(2)).unwrap();
        }
        let mut wal = WalManager::open(&path, true).unwrap();
        let records = wal.read_all_records().unwrap();
        assert_eq!(records.iter().map(|record| record.commit_version).collect::<Vec<_>>(), vec![1, 2]);
    }

    #[test]
    fn interrupted_file_replacement_restores_backup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("replace.db");
        {
            let store = MVStore::open(&path).unwrap();
            store.open_map("items").put(b"key".to_vec(), b"value".to_vec());
            store.sync().unwrap();
        }
        std::fs::rename(&path, path.with_extension("compact_bak")).unwrap();
        let store = MVStore::open(&path).unwrap();
        assert_eq!(store.open_map("items").get(b"key"), Some(b"value".to_vec()));
    }

    #[test]
    fn checkpoints_interleave_with_distinct_commits_without_losing_data() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("interleaved.db");
        let store = Arc::new(MVStore::open(&path).unwrap());
        let tx_store = TransactionStore::new(Arc::clone(&store));
        let stop = Arc::new(AtomicBool::new(false));
        let checkpointer = {
            let store = Arc::clone(&store);
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    store.checkpoint_if_needed(1, std::time::Duration::ZERO).unwrap();
                    std::thread::yield_now();
                }
            })
        };
        let workers: Vec<_> = (0..4u8).map(|worker| {
            let tx_store = Arc::clone(&tx_store);
            std::thread::spawn(move || {
                for value in 0..50u8 {
                    let tx = tx_store.begin();
                    tx.put("interleaved", vec![worker], vec![value]).unwrap();
                    tx.commit().unwrap();
                }
            })
        }).collect();
        for worker in workers {
            worker.join().unwrap();
        }
        stop.store(true, Ordering::Relaxed);
        checkpointer.join().unwrap();
        store.sync().unwrap();
        drop(tx_store);
        drop(store);

        let store = Arc::new(MVStore::open(&path).unwrap());
        let tx_store = TransactionStore::new(store);
        let tx = tx_store.begin();
        for worker in 0..4u8 {
            assert_eq!(tx.get("interleaved", &[worker]).unwrap(), Some(vec![49]));
        }
    }

    #[test]
    fn delta_checkpoints_preserve_map_lifecycle_changes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("maps.db");
        {
            let store = MVStore::open(&path).unwrap();
            store.open_map("old").put(b"key".to_vec(), b"value".to_vec());
            store.sync().unwrap();
            store.rename_map("old", "renamed").unwrap();
            store.sync().unwrap();
            store.clear_map("renamed");
            store.open_map("new").put(b"another".to_vec(), b"item".to_vec());
            store.sync().unwrap();
        }
        {
            let store = MVStore::open(&path).unwrap();
            assert!(!store.get_map_names().contains(&"old".to_string()));
            assert_eq!(store.open_map("renamed").get(b"key"), None);
            assert_eq!(store.open_map("new").get(b"another"), Some(b"item".to_vec()));
            assert!(store.remove_map("renamed"));
            store.sync().unwrap();
        }
        let store = MVStore::open(&path).unwrap();
        assert!(!store.get_map_names().contains(&"renamed".to_string()));
        assert_eq!(store.open_map("new").get(b"another"), Some(b"item".to_vec()));
    }

    #[test]
    fn checkpoint_recovers_from_one_corrupt_header_slot() {
        use std::io::{Seek, SeekFrom, Write};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("headers.db");
        {
            let store = MVStore::open(&path).unwrap();
            let map = store.open_map("items");
            map.put(b"key".to_vec(), b"first".to_vec());
            store.sync().unwrap();
            map.put(b"key".to_vec(), b"second".to_vec());
            store.sync().unwrap();
        }
        {
            let mut file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
            file.write_all(&[0, 0, 0, 0]).unwrap();
            file.sync_all().unwrap();
        }
        {
            let store = MVStore::open(&path).unwrap();
            assert_eq!(store.open_map("items").get(b"key"), Some(b"second".to_vec()));
            store.sync().unwrap();
        }
        {
            let mut file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
            file.seek(SeekFrom::Start(2048)).unwrap();
            file.write_all(&[0, 0, 0, 0]).unwrap();
            file.sync_all().unwrap();
        }
        let store = MVStore::open(&path).unwrap();
        assert_eq!(store.open_map("items").get(b"key"), Some(b"second".to_vec()));
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
        tx_store.set_lock_timeout_ms(50);

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

    #[test]
    fn test_deadlock_detection_and_cancellation() {
        let store = Arc::new(MVStore::open_in_memory());
        let tx_store = TransactionStore::new(store);
        tx_store.set_lock_timeout_ms(2000);

        let tx1 = Arc::new(tx_store.begin());
        let tx2 = Arc::new(tx_store.begin());

        // Step 1: Tx1 が keyA を更新 (Tx1 が keyA をロック)
        tx1.put("test", b"keyA".to_vec(), b"valA1".to_vec()).unwrap();

        // Step 2: Tx2 が keyB を更新 (Tx2 が keyB をロック)
        tx2.put("test", b"keyB".to_vec(), b"valB1".to_vec()).unwrap();

        // Step 3: 別スレッドで Tx1 が keyB を更新しようとする -> Tx2 がロック中なので待機に入る
        let tx1_clone = Arc::clone(&tx1);
        let h1 = std::thread::spawn(move || {
            tx1_clone.put("test", b"keyB".to_vec(), b"valB_by_tx1".to_vec())
        });

        // Tx1 が待機に入るのを少し待つ
        std::thread::sleep(std::time::Duration::from_millis(50));

        // Step 4: メインスレッドで Tx2 が keyA を更新しようとする
        // -> Tx2 は keyA (Tx1保持) を要求 -> Tx1 は既に keyB (Tx2保持) を要求して待機中
        // -> デッドロックが検出され、Tx2 がキャンセル（自動ロールバック）される！
        let res2 = tx2.put("test", b"keyA".to_vec(), b"valA_by_tx2".to_vec());

        assert!(res2.is_err(), "Tx2 should fail due to deadlock detection");
        let err_msg = res2.unwrap_err().to_string();
        assert!(
            err_msg.contains("Deadlock detected"),
            "Error should indicate deadlock: {}",
            err_msg
        );

        // Tx2 がキャンセル（ロールバック）されたことにより、Tx2 の保持していた keyB のロックが解放され、
        // 待機していた Tx1 の put が完了する
        let res1 = h1.join().unwrap();
        assert!(res1.is_ok(), "Tx1 should successfully acquire lock and complete put after Tx2 rollback: {:?}", res1);

        // Tx1 は正常にコミットできる
        assert!(tx1.commit().is_ok());

        // コミット後の状態を確認
        let tx3 = tx_store.begin();
        assert_eq!(tx3.get("test", b"keyA").unwrap(), Some(b"valA1".to_vec()));
        assert_eq!(tx3.get("test", b"keyB").unwrap(), Some(b"valB_by_tx1".to_vec()));
    }
}
