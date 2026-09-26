use std::fs;
use h2::{Connection, Value};

#[test]
fn test_iceberg_ddl_and_initialization() {
    let tmp_dir = std::env::temp_dir().join(format!("h2_iceberg_test_ddl_{}", uuid::Uuid::new_v4()));
    let loc = tmp_dir.to_string_lossy().to_string().replace('\\', "/");

    let conn = Connection::open_in_memory().unwrap();

    let ddl = format!(
        "CREATE EXTERNAL TABLE lake_products (
            id BIGINT PRIMARY KEY,
            name VARCHAR(64),
            price DOUBLE,
            category VARCHAR(32)
        ) STORED AS ICEBERG
        LOCATION '{}';",
        loc
    );

    conn.execute(&ddl).unwrap();

    // metadata/version-hint.text および metadata/v1.metadata.json が生成されたか検証
    let hint_file = tmp_dir.join("metadata").join("version-hint.text");
    assert!(hint_file.exists());
    let hint_content = fs::read_to_string(&hint_file).unwrap();
    assert_eq!(hint_content.trim(), "1");

    let v1_meta = tmp_dir.join("metadata").join("v1.metadata.json");
    assert!(v1_meta.exists());

    // 初期状態のテーブルは 0 件
    let rows = conn.query("SELECT * FROM lake_products;").unwrap();
    assert_eq!(rows.len(), 0);

    let _ = fs::remove_dir_all(&tmp_dir);
}

#[test]
fn test_iceberg_insert_and_select() {
    let tmp_dir = std::env::temp_dir().join(format!("h2_iceberg_test_crud_{}", uuid::Uuid::new_v4()));
    let loc = tmp_dir.to_string_lossy().to_string().replace('\\', "/");

    let conn = Connection::open_in_memory().unwrap();

    let ddl = format!(
        "CREATE EXTERNAL TABLE lake_items (
            id BIGINT,
            name VARCHAR(64),
            price DOUBLE,
            category VARCHAR(32)
        ) STORED AS ICEBERG
        LOCATION '{}';",
        loc
    );
    conn.execute(&ddl).unwrap();

    // 挿入実行
    let inserted = conn.execute(
        "INSERT INTO lake_items (id, name, price, category) VALUES
            (1, 'Book A', 15.5, 'Books'),
            (2, 'Laptop B', 1200.0, 'Electronics');"
    ).unwrap();
    assert_eq!(inserted, 2);

    // Parquet ファイルが生成されたか検証
    let data_dir = tmp_dir.join("data");
    assert!(data_dir.exists());
    let parquet_files: Vec<_> = fs::read_dir(&data_dir).unwrap().collect();
    assert_eq!(parquet_files.len(), 1);

    // version-hint が "2" に更新されたか検証
    let hint_file = tmp_dir.join("metadata").join("version-hint.text");
    assert_eq!(fs::read_to_string(&hint_file).unwrap().trim(), "2");

    // クエリ実行
    let rows = conn.query("SELECT id, name, price, category FROM lake_items ORDER BY id;").unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].get(0), Some(&Value::BigInt(1)));
    assert_eq!(rows[0].get(1), Some(&Value::String("Book A".to_string())));
    assert_eq!(rows[0].get(2), Some(&Value::Double(15.5)));
    assert_eq!(rows[0].get(3), Some(&Value::String("Books".to_string())));

    assert_eq!(rows[1].get(0), Some(&Value::BigInt(2)));
    assert_eq!(rows[1].get(1), Some(&Value::String("Laptop B".to_string())));
    assert_eq!(rows[1].get(2), Some(&Value::Double(1200.0)));

    let _ = fs::remove_dir_all(&tmp_dir);
}

#[test]
fn test_iceberg_multiple_commits_and_accumulation() {
    let tmp_dir = std::env::temp_dir().join(format!("h2_iceberg_test_commits_{}", uuid::Uuid::new_v4()));
    let loc = tmp_dir.to_string_lossy().to_string().replace('\\', "/");

    let conn = Connection::open_in_memory().unwrap();

    let ddl = format!(
        "CREATE EXTERNAL TABLE store_lake (
            id BIGINT,
            name VARCHAR(64),
            qty INT
        ) STORED AS ICEBERG
        LOCATION '{}';",
        loc
    );
    conn.execute(&ddl).unwrap();

    // コミット 1
    conn.execute("INSERT INTO store_lake (id, name, qty) VALUES (1, 'Item 1', 10);").unwrap();

    // コミット 2
    conn.execute("INSERT INTO store_lake (id, name, qty) VALUES (2, 'Item 2', 20), (3, 'Item 3', 30);").unwrap();

    // 合計 3 件
    let rows = conn.query("SELECT id, name, qty FROM store_lake ORDER BY id;").unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].get(0), Some(&Value::BigInt(1)));
    assert_eq!(rows[1].get(0), Some(&Value::BigInt(2)));
    assert_eq!(rows[2].get(0), Some(&Value::BigInt(3)));

    // 集計クエリの動作検証
    let count_rows = conn.query("SELECT COUNT(*), SUM(qty) FROM store_lake;").unwrap();
    assert_eq!(count_rows[0].get(0), Some(&Value::BigInt(3)));
    assert_eq!(count_rows[0].get(1), Some(&Value::BigInt(60)));

    let _ = fs::remove_dir_all(&tmp_dir);
}

#[test]
fn test_iceberg_data_skipping_and_filter() {
    let tmp_dir = std::env::temp_dir().join(format!("h2_iceberg_test_skip_{}", uuid::Uuid::new_v4()));
    let loc = tmp_dir.to_string_lossy().to_string().replace('\\', "/");

    let conn = Connection::open_in_memory().unwrap();

    let ddl = format!(
        "CREATE EXTERNAL TABLE metrics_lake (
            ts BIGINT,
            metric_val DOUBLE
        ) STORED AS ICEBERG
        LOCATION '{}';",
        loc
    );
    conn.execute(&ddl).unwrap();

    // ファイル 1: ts in [100, 200]
    conn.execute("INSERT INTO metrics_lake VALUES (100, 1.0), (200, 2.0);").unwrap();
    // ファイル 2: ts in [300, 400]
    conn.execute("INSERT INTO metrics_lake VALUES (300, 3.0), (400, 4.0);").unwrap();

    // 条件検索
    let r1 = conn.query("SELECT metric_val FROM metrics_lake WHERE ts = 100;").unwrap();
    assert_eq!(r1.len(), 1);
    assert_eq!(r1[0].get(0), Some(&Value::Double(1.0)));

    // 範囲検索（Min/Max 枝刈り連携）
    let r2 = conn.query("SELECT metric_val FROM metrics_lake WHERE ts > 250 ORDER BY ts;").unwrap();
    assert_eq!(r2.len(), 2);
    assert_eq!(r2[0].get(0), Some(&Value::Double(3.0)));
    assert_eq!(r2[1].get(0), Some(&Value::Double(4.0)));

    // 存在しない値
    let r3 = conn.query("SELECT metric_val FROM metrics_lake WHERE ts = 999;").unwrap();
    assert_eq!(r3.len(), 0);

    let _ = fs::remove_dir_all(&tmp_dir);
}

#[test]
fn test_iceberg_time_travel_and_inspection() {
    let tmp_dir = std::env::temp_dir().join(format!("h2_iceberg_test_tt_{}", uuid::Uuid::new_v4()));
    let loc = tmp_dir.to_string_lossy().to_string().replace('\\', "/");

    let conn = Connection::open_in_memory().unwrap();

    let ddl = format!(
        "CREATE EXTERNAL TABLE event_lake (
            id BIGINT,
            event_name VARCHAR(32)
        ) STORED AS ICEBERG
        LOCATION '{}';",
        loc
    );
    conn.execute(&ddl).unwrap();

    // コミット 1
    conn.execute("INSERT INTO event_lake VALUES (1, 'Login');").unwrap();

    // スナップショット一覧を取得
    let snaps1 = conn.query("SELECT snapshot_id, added_records, total_records FROM iceberg_snapshots('event_lake');").unwrap();
    assert_eq!(snaps1.len(), 1);
    let first_snap_id = match snaps1[0].get(0).unwrap() {
        Value::BigInt(id) => *id,
        _ => panic!("Expected BigInt snapshot_id"),
    };
    assert_eq!(snaps1[0].get(1), Some(&Value::BigInt(1)));
    assert_eq!(snaps1[0].get(2), Some(&Value::BigInt(1)));

    // コミット 2
    conn.execute("INSERT INTO event_lake VALUES (2, 'Purchase'), (3, 'Logout');").unwrap();

    // 最新状態は 3 件
    let current_rows = conn.query("SELECT id, event_name FROM event_lake ORDER BY id;").unwrap();
    assert_eq!(current_rows.len(), 3);

    // スナップショット一覧が 2 件に増えていることを検証
    let snaps2 = conn.query("SELECT snapshot_id, total_records FROM iceberg_snapshots('event_lake') ORDER BY snapshot_id;").unwrap();
    assert_eq!(snaps2.len(), 2);
    assert_eq!(snaps2[1].get(1), Some(&Value::BigInt(3)));

    // タイムトラベル: 第1スナップショット時点のデータを参照 -> 1件のみ
    let tt_query = format!("SELECT id, event_name FROM iceberg_scan('event_lake', {});", first_snap_id);
    let tt_rows = conn.query(&tt_query).unwrap();
    assert_eq!(tt_rows.len(), 1);
    assert_eq!(tt_rows[0].get(0), Some(&Value::BigInt(1)));
    assert_eq!(tt_rows[0].get(1), Some(&Value::String("Login".to_string())));

    // ファイル検査関数の検証
    let files = conn.query("SELECT file_path, file_format, record_count FROM iceberg_files('event_lake');").unwrap();
    assert_eq!(files.len(), 2);
    assert_eq!(files[0].get(1), Some(&Value::String("PARQUET".to_string())));

    let _ = fs::remove_dir_all(&tmp_dir);
}

#[test]
fn test_iceberg_join_with_relational_table() {
    let tmp_dir = std::env::temp_dir().join(format!("h2_iceberg_test_join_{}", uuid::Uuid::new_v4()));
    let loc = tmp_dir.to_string_lossy().to_string().replace('\\', "/");

    let conn = Connection::open_in_memory().unwrap();

    // Iceberg 外部テーブル
    let ddl = format!(
        "CREATE TABLE lake_books (
            book_id BIGINT,
            title VARCHAR(64),
            author_id INT,
            price DOUBLE
        ) WITH (
            TYPE = 'ICEBERG',
            LOCATION = '{}'
        );",
        loc
    );
    conn.execute(&ddl).unwrap();

    conn.execute(
        "INSERT INTO lake_books VALUES
            (1, 'Rust in Action', 101, 45.0),
            (2, 'Database Internals', 102, 55.0),
            (3, 'Designing Data-Intensive Applications', 103, 50.0);"
    ).unwrap();

    // 通常のローカルリレーショナルテーブル
    conn.execute(
        "CREATE TABLE authors (
            author_id INT PRIMARY KEY,
            author_name VARCHAR(64),
            country VARCHAR(32)
        );"
    ).unwrap();

    conn.execute(
        "INSERT INTO authors VALUES
            (101, 'Tim McNamara', 'NZ'),
            (102, 'Alex Petrov', 'DE'),
            (103, 'Martin Kleppmann', 'UK');"
    ).unwrap();

    // 透過的 JOIN クエリ
    let join_rows = conn.query(
        "SELECT b.book_id, b.title, a.author_name, a.country, b.price
         FROM lake_books b
         JOIN authors a ON b.author_id = a.author_id
         ORDER BY b.book_id;"
    ).unwrap();

    assert_eq!(join_rows.len(), 3);
    assert_eq!(join_rows[0].get(1), Some(&Value::String("Rust in Action".to_string())));
    assert_eq!(join_rows[0].get(2), Some(&Value::String("Tim McNamara".to_string())));
    assert_eq!(join_rows[0].get(3), Some(&Value::String("NZ".to_string())));

    assert_eq!(join_rows[1].get(1), Some(&Value::String("Database Internals".to_string())));
    assert_eq!(join_rows[1].get(2), Some(&Value::String("Alex Petrov".to_string())));

    assert_eq!(join_rows[2].get(1), Some(&Value::String("Designing Data-Intensive Applications".to_string())));
    assert_eq!(join_rows[2].get(2), Some(&Value::String("Martin Kleppmann".to_string())));

    let _ = fs::remove_dir_all(&tmp_dir);
}

#[test]
fn test_iceberg_infer_existing_table_schema() {
    let tmp_dir = std::env::temp_dir().join(format!("h2_iceberg_test_infer_{}", uuid::Uuid::new_v4()));
    let loc = tmp_dir.to_string_lossy().to_string().replace('\\', "/");

    let conn1 = Connection::open_in_memory().unwrap();

    // 接続1でスキーマを定義して作成し、データ投入
    let ddl1 = format!(
        "CREATE EXTERNAL TABLE original_lake (
            id BIGINT,
            product VARCHAR,
            cost DOUBLE
        ) STORED AS ICEBERG
        LOCATION '{}';",
        loc
    );
    conn1.execute(&ddl1).unwrap();
    conn1.execute("INSERT INTO original_lake VALUES (10, 'Desk', 150.0);").unwrap();

    // 別接続（新しいDBセッション）から、スキーマ列定義を省略して既存の Iceberg テーブルを指定
    let conn2 = Connection::open_in_memory().unwrap();
    let ddl2 = format!(
        "CREATE EXTERNAL TABLE inferred_lake
        STORED AS ICEBERG
        LOCATION '{}';",
        loc
    );
    conn2.execute(&ddl2).unwrap();

    // スキーマが自動推論され、データが参照できることを検証
    let rows = conn2.query("SELECT id, product, cost FROM inferred_lake;").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get(0), Some(&Value::BigInt(10)));
    assert_eq!(rows[0].get(1), Some(&Value::String("Desk".to_string())));
    assert_eq!(rows[0].get(2), Some(&Value::Double(150.0)));

    let _ = fs::remove_dir_all(&tmp_dir);
}
