use tempfile::tempdir;
use h2::{Connection, Instance, InstanceConfig, RecoveryTarget, Value};

#[test]
fn test_binary_backup_format_and_verification() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("primary.db");
    let backup_path = dir.path().join("base_backup.h2bk");
    let backup_str = backup_path.to_str().unwrap().replace('\\', "/");

    let conn = Connection::open(&db_path).unwrap();
    conn.execute("CREATE TABLE users (id INT PRIMARY KEY, name VARCHAR(50), balance INT);").unwrap();
    conn.execute("INSERT INTO users VALUES (1, 'Alice', 1000), (2, 'Bob', 2000);").unwrap();

    // 1. バイナリバックアップの作成 (SQL)
    conn.execute(&format!("BACKUP TO '{}';", backup_str)).unwrap();

    // ファイル先頭が H2BK マジックであることを検証
    let file_bytes = std::fs::read(&backup_path).unwrap();
    assert!(file_bytes.len() > 64);
    assert_eq!(&file_bytes[0..4], b"H2BK");
    assert_eq!(&file_bytes[file_bytes.len() - 4..], b"H2EF");

    // 2. RESTORE VERIFYONLY (SQL)
    let verify_rows = conn.query(&format!("RESTORE VERIFYONLY FROM '{}';", backup_str)).unwrap();
    assert_eq!(verify_rows.len(), 1);
    assert_eq!(verify_rows[0].get(6).unwrap(), &Value::String("VERIFIED".to_string()));
    assert_eq!(verify_rows[0].get(5).unwrap(), &Value::Boolean(false)); // not replica

    // 3. データ変更後に復元
    conn.execute("UPDATE users SET balance = 9999 WHERE id = 1;").unwrap();
    conn.execute("DELETE FROM users WHERE id = 2;").unwrap();
    let count_before = conn.query("SELECT COUNT(*) FROM users;").unwrap();
    assert_eq!(count_before[0].get(0).unwrap(), &Value::BigInt(1));

    conn.execute(&format!("RESTORE FROM '{}';", backup_str)).unwrap();

    let rows = conn.query("SELECT id, name, balance FROM users ORDER BY id;").unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].get(1).unwrap(), &Value::String("Alice".to_string()));
    assert_eq!(rows[0].get(2).unwrap(), &Value::Integer(1000));
    assert_eq!(rows[1].get(1).unwrap(), &Value::String("Bob".to_string()));
    assert_eq!(rows[1].get(2).unwrap(), &Value::Integer(2000));
}

#[test]
fn test_deprecated_json_backup_rejection() {
    let dir = tempdir().unwrap();
    let json_backup_path = dir.path().join("legacy.json");

    // JSON フォーマットのファイルを作成
    let json_content = r#"{"items": [[1, 2]]}"#;
    std::fs::write(&json_backup_path, json_content).unwrap();

    let conn = Connection::open_in_memory().unwrap();

    // 1. verify_backup は JSON ファイルを不正なマジックとして拒否する
    let verify_err = conn.verify_backup(&json_backup_path).unwrap_err();
    assert!(
        verify_err.to_string().contains("Invalid backup magic"),
        "Expected Invalid backup magic error, got: {}",
        verify_err
    );

    // 2. restore_from も JSON ファイルを拒否する
    let restore_err = conn.restore_from(&json_backup_path).unwrap_err();
    assert!(
        restore_err.to_string().contains("Invalid backup magic"),
        "Expected Invalid backup magic error, got: {}",
        restore_err
    );
}

#[test]
fn test_corrupted_backup_detection() {
    let dir = tempdir().unwrap();
    let backup_path = dir.path().join("corrupt_test.h2bk");
    let backup_str = backup_path.to_str().unwrap().replace('\\', "/");

    let conn = Connection::open_in_memory().unwrap();
    conn.execute("CREATE TABLE orders (id INT PRIMARY KEY, amount INT);").unwrap();
    conn.execute("INSERT INTO orders VALUES (101, 500);").unwrap();
    conn.backup(&backup_path).unwrap();

    // 故意に末尾のトレイラー直前を反転させて破損させる
    let mut data = std::fs::read(&backup_path).unwrap();
    let corrupt_pos = data.len() - 30;
    data[corrupt_pos] ^= 0xFF;
    std::fs::write(&backup_path, &data).unwrap();

    // RESTORE VERIFYONLY が破損を検出してエラーになることを検証
    let verify_res = conn.execute_raw(&format!("RESTORE VERIFYONLY FROM '{}';", backup_str));
    assert!(verify_res.is_err(), "Corrupted backup must be rejected by VERIFYONLY");

    // RESTORE FROM も破損を検出して拒否することを確認
    let restore_res = conn.restore_from(&backup_path);
    assert!(restore_res.is_err(), "Corrupted backup must be rejected by restore_from");
}

#[test]
fn test_pitr_continuous_wal_archiving() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("pitr_prod.db");
    let archive_dir = dir.path().join("wal_archive");
    let archive_str = archive_dir.to_str().unwrap().replace('\\', "/");
    let base_backup_path = dir.path().join("base_backup.h2bk");
    let base_backup_str = base_backup_path.to_str().unwrap().replace('\\', "/");

    // 1. 本番 DB を起動し、継続的 WAL アーカイブを有効化
    let conn = Connection::open(&db_path).unwrap();
    conn.enable_wal_archiver(&archive_dir).unwrap();

    conn.execute("CREATE TABLE logs (step INT PRIMARY KEY, msg VARCHAR(100));").unwrap();
    conn.execute("INSERT INTO logs VALUES (1, 'Initial System Setup');").unwrap();

    // ベースバックアップを取得
    let base_meta = conn.backup(&base_backup_path).unwrap();
    let base_version = base_meta.snapshot_version;

    // 追加トランザクションを順次コミット (WAL アーカイブへ自動退避される)
    conn.execute("INSERT INTO logs VALUES (2, 'User Registration Started');").unwrap();
    let v2 = conn.query("SELECT COUNT(*) FROM logs;").unwrap();
    assert_eq!(v2[0].get(0).unwrap(), &Value::BigInt(2));

    conn.execute("INSERT INTO logs VALUES (3, 'Payment Gateway Linked');").unwrap();
    conn.execute("INSERT INTO logs VALUES (4, 'ACCIDENTAL DISASTER DROP TABLE');").unwrap();

    // 2. PITR 復元: step=2 のコミットバージョンまで巻き戻す
    // 新規リカバリ用 DB インスタンスを開く
    let restore_db_path = dir.path().join("pitr_recovery.db");
    let restore_conn = Connection::open(&restore_db_path).unwrap();

    // ベースバックアップのスナップショットバージョンより1つ進んだコミット（step=2）へ復元
    let target_version = base_version + 1;
    let report = restore_conn
        .restore_pitr(&base_backup_path, Some(&archive_dir), &RecoveryTarget::Version(target_version))
        .unwrap();

    assert_eq!(report.base_snapshot_version, base_version);
    assert_eq!(report.final_recovered_version, target_version);
    assert_eq!(report.records_replayed, 1);

    // 復旧された DB の状態を確認: step=1, 2 のみが存在し、3 と 4 は存在しない！
    let restored_rows = restore_conn.query("SELECT step, msg FROM logs ORDER BY step;").unwrap();
    assert_eq!(restored_rows.len(), 2);
    assert_eq!(restored_rows[0].get(0).unwrap(), &Value::Integer(1));
    assert_eq!(restored_rows[1].get(0).unwrap(), &Value::Integer(2));
    assert_eq!(restored_rows[1].get(1).unwrap(), &Value::String("User Registration Started".to_string()));

    // 3. SQL 経由での PITR (WITH WAL_ARCHIVE = '...', RECOVERY_TARGET_VERSION = ...)
    let restore_sql_db = dir.path().join("pitr_sql.db");
    let sql_conn = Connection::open(&restore_sql_db).unwrap();

    let sql = format!(
        "RESTORE FROM '{}' WITH WAL_ARCHIVE = '{}', RECOVERY_TARGET_VERSION = {};",
        base_backup_str, archive_str, base_version + 2
    );
    sql_conn.execute(&sql).unwrap();

    let sql_rows = sql_conn.query("SELECT step, msg FROM logs ORDER BY step;").unwrap();
    assert_eq!(sql_rows.len(), 3);
    assert_eq!(sql_rows[2].get(0).unwrap(), &Value::Integer(3));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_replica_backup_and_restore() {
    let dir = tempdir().unwrap();
    let primary_db = dir.path().join("primary_replica_test.db");
    let standby_db = dir.path().join("standby_replica_test.db");
    let replica_backup_path = dir.path().join("replica_backup.h2bk");

    // 1. Primary を起動 (ポート 0 でバインド)
    let addr: std::net::SocketAddr = "127.0.0.1:0".parse().unwrap();
    let primary_cfg = InstanceConfig::primary(addr);
    let primary = Instance::open(&primary_db, primary_cfg).unwrap();
    let primary_listen_addr = primary.replication_addr().unwrap();

    // 2. Standby を起動
    let standby_cfg = InstanceConfig::standby(primary_listen_addr);
    let standby = Instance::open(&standby_db, standby_cfg).unwrap();

    // Primary に書き込み (同期レプリケーションでスタンバイへ即座に反映)
    let p_conn = primary.connect().unwrap();
    p_conn.execute("CREATE TABLE products (id INT PRIMARY KEY, name VARCHAR(50), stock INT);").unwrap();
    p_conn.execute("INSERT INTO products VALUES (1, 'Laptop', 10), (2, 'Monitor', 25);").unwrap();

    // Standby が Read-Only であることを確認
    assert!(standby.is_read_only());
    let s_conn = standby.connect().unwrap();
    let s_rows = s_conn.query("SELECT id, name, stock FROM products ORDER BY id;").unwrap();
    assert_eq!(s_rows.len(), 2);

    // 3. リードレプリカ (Standby) から整合バックアップを取得！
    let meta = standby.backup(&replica_backup_path).unwrap();
    assert!(meta.is_replica, "Metadata must mark backup as taken from replica");
    assert!(meta.total_records >= 2);

    // バックアップファイルの整合性検証
    let verified = standby.verify_backup(&replica_backup_path).unwrap();
    assert_eq!(verified.checksum, meta.checksum);

    // 4. レプリカバックアップから新規インスタンスへ復元
    let restored_db = dir.path().join("restored_from_replica.db");
    let restored_conn = Connection::open(&restored_db).unwrap();
    restored_conn.restore_from(&replica_backup_path).unwrap();

    let restored_rows = restored_conn.query("SELECT id, name, stock FROM products ORDER BY id;").unwrap();
    assert_eq!(restored_rows.len(), 2);
    assert_eq!(restored_rows[0].get(1).unwrap(), &Value::String("Laptop".to_string()));
    assert_eq!(restored_rows[1].get(1).unwrap(), &Value::String("Monitor".to_string()));

    primary.close();
    standby.close();
}
