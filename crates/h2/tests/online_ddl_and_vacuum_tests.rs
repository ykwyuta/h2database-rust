use h2::{Connection, Value};
use std::time::Instant;
use tempfile::NamedTempFile;

#[test]
fn test_instant_add_and_drop_column_performance_and_correctness() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute("CREATE TABLE users (id INTEGER PRIMARY KEY, name VARCHAR)").unwrap();

    // 1. 1,000行のデータを挿入
    conn.execute("BEGIN").unwrap();
    for i in 1..=1000 {
        conn.execute(&format!("INSERT INTO users VALUES ({}, 'user_{}')", i, i)).unwrap();
    }
    conn.execute("COMMIT").unwrap();

    let initial_count = conn.query("SELECT COUNT(*) FROM users").unwrap();
    assert_eq!(initial_count[0].get(0), Some(&Value::BigInt(1000)));

    // 2. Instant Add Column の実行（全行物理書き換えを行わないため、数千〜数万行でも一瞬で完了）
    let start = Instant::now();
    conn.execute("ALTER TABLE users ADD COLUMN age INTEGER").unwrap();
    let duration = start.elapsed();
    println!("Instant Add Column duration: {:?}", duration);
    // Instant DDL であるため、全行スキャン書き直しなしで即座に完了する（デバッグモードでも500ms未満）
    assert!(duration.as_millis() < 500, "Instant add column should complete quickly, took {:?}", duration);

    // 3. 既存行の読み出しで追加カラムが透過的に NULL として補完されることを検証
    let rows = conn.query("SELECT id, name, age FROM users WHERE id <= 3 ORDER BY id").unwrap();
    assert_eq!(rows.len(), 3);
    for row in &rows {
        assert_eq!(row.get(2), Some(&Value::Null), "Existing rows must have NULL for newly added column");
    }

    // 4. 新規行の挿入（新スキーマに対応）
    conn.execute("INSERT INTO users VALUES (1001, 'new_user', 30)").unwrap();
    let new_row = conn.query("SELECT id, name, age FROM users WHERE id = 1001").unwrap();
    assert_eq!(new_row.len(), 1);
    assert_eq!(new_row[0].get(2), Some(&Value::Integer(30)));

    // 5. UPDATE 文で新旧行の追加カラムを正常に更新できることを検証
    conn.execute("UPDATE users SET age = 25 WHERE id = 1").unwrap();
    let updated_row = conn.query("SELECT id, name, age FROM users WHERE id = 1").unwrap();
    assert_eq!(updated_row[0].get(2), Some(&Value::Integer(25)));

    // 6. Instant Drop Column の実行
    let start_drop = Instant::now();
    conn.execute("ALTER TABLE users DROP COLUMN age").unwrap();
    let duration_drop = start_drop.elapsed();
    println!("Instant Drop Column duration: {:?}", duration_drop);
    assert!(duration_drop.as_millis() < 500, "Instant drop column should complete in < 500ms, took {:?}", duration_drop);

    // 7. 削除列が非表示になり、残りのカラムが正常にクエリできることを検証
    let rows_after_drop = conn.query("SELECT * FROM users WHERE id IN (1, 1001) ORDER BY id").unwrap();
    assert_eq!(rows_after_drop.len(), 2);
    // カラム数は id, name の 2つ
    assert_eq!(rows_after_drop[0].values.len(), 2);
    assert_eq!(rows_after_drop[0].get(1), Some(&Value::String("user_1".to_string())));
    assert_eq!(rows_after_drop[1].get(1), Some(&Value::String("new_user".to_string())));
}

#[test]
fn test_online_rename_and_truncate_performance_and_correctness() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute("CREATE TABLE products (id INTEGER PRIMARY KEY, title VARCHAR)").unwrap();
    conn.execute("CREATE INDEX idx_prod_title ON products (title)").unwrap();

    // 1. 1,000行挿入
    conn.execute("BEGIN").unwrap();
    for i in 1..=1000 {
        conn.execute(&format!("INSERT INTO products VALUES ({}, 'product_{}')", i, i)).unwrap();
    }
    conn.execute("COMMIT").unwrap();

    // 2. Online Rename Table の実行（全行コピーではなく O(1) マップ置換）
    let start_rename = Instant::now();
    conn.execute("ALTER TABLE products RENAME TO items").unwrap();
    let duration_rename = start_rename.elapsed();
    println!("Online Rename Table duration: {:?}", duration_rename);
    assert!(duration_rename.as_millis() < 500, "Online rename table should complete in < 500ms, took {:?}", duration_rename);

    // 3. 新テーブル名でのクエリ（インデックススキャン含む）
    let renamed_rows = conn.query("SELECT id, title FROM items WHERE title = 'product_100'").unwrap();
    assert_eq!(renamed_rows.len(), 1);
    assert_eq!(renamed_rows[0].get(0), Some(&Value::Integer(100)));

    // 4. Online Truncate Table の実行（1行ずつの削除ではなく O(1) 一括クリア）
    let start_truncate = Instant::now();
    conn.execute("TRUNCATE TABLE items").unwrap();
    let duration_truncate = start_truncate.elapsed();
    println!("Online Truncate Table duration: {:?}", duration_truncate);
    assert!(duration_truncate.as_millis() < 500, "Online truncate table should complete in < 500ms, took {:?}", duration_truncate);

    // 5. テーブルが空になり、次回のINSERTでrow_idが1から採番されること
    let empty_count = conn.query("SELECT COUNT(*) FROM items").unwrap();
    assert_eq!(empty_count[0].get(0), Some(&Value::BigInt(0)));

    conn.execute("INSERT INTO items VALUES (1, 'fresh_item')").unwrap();
    let fresh = conn.query("SELECT id, title FROM items").unwrap();
    assert_eq!(fresh.len(), 1);
    assert_eq!(fresh[0].get(1), Some(&Value::String("fresh_item".to_string())));
}

#[test]
fn test_create_index_concurrently_and_query() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute("CREATE TABLE orders (id INTEGER, customer VARCHAR, amount INTEGER)").unwrap();

    conn.execute("BEGIN").unwrap();
    for i in 1..=1000 {
        conn.execute(&format!("INSERT INTO orders VALUES ({}, 'cust_{}', {})", i, i % 10, i * 10)).unwrap();
    }
    conn.execute("COMMIT").unwrap();

    // CREATE INDEX CONCURRENTLY の実行
    conn.execute("CREATE INDEX CONCURRENTLY idx_orders_cust ON orders (customer)").unwrap();

    // インデックス作成後に条件検索が正確に行えることを検証
    let rows = conn.query("SELECT id, customer, amount FROM orders WHERE customer = 'cust_5'").unwrap();
    assert_eq!(rows.len(), 100);
}

#[test]
fn test_online_vacuum_with_concurrent_transactions() {
    let temp_file = NamedTempFile::new().unwrap();
    let db_path = temp_file.path().to_path_buf();

    // 1. 接続1を作成して初期データをコミット
    let conn1 = Connection::open(&db_path).unwrap();
    conn1.execute("CREATE TABLE logs (id INTEGER PRIMARY KEY, msg VARCHAR)").unwrap();

    conn1.execute("BEGIN").unwrap();
    for i in 1..=200 {
        conn1.execute(&format!("INSERT INTO logs VALUES ({}, 'Committed log message {}')", i, i)).unwrap();
    }
    conn1.execute("COMMIT").unwrap();

    let initial_size = std::fs::metadata(&db_path).unwrap().len();

    // 2. 一部データを削除してコミット（死にレコードを作成）
    conn1.execute("DELETE FROM logs WHERE id > 50").unwrap();

    // 3. セッション2（並行接続）を開始し、未コミットの変更を書き込んだまま保持（Open状態）
    let conn2 = conn1.new_session();
    conn2.execute("BEGIN").unwrap();
    conn2.execute("INSERT INTO logs VALUES (9999, 'Uncommitted in-flight message')").unwrap();

    // 4. セッション1でオンライン Vacuum を実行！
    // 並行トランザクション（conn2）が未コミットのまま走っている状況でも、
    // ブロックすることなく安全に未コミットデータを除外してコミット済みデータのみを Vacuum する。
    conn1.execute("VACUUM").unwrap();

    let post_vacuum_size = std::fs::metadata(&db_path).unwrap().len();
    println!("Initial size: {}, Post-vacuum size: {}", initial_size, post_vacuum_size);
    assert!(post_vacuum_size < initial_size, "Vacuum should reduce file size by purging deleted records");

    // 5. セッション2をロールバック（未コミット変更の破棄）
    conn2.execute("ROLLBACK").unwrap();

    // 6. 現在の接続でコミット済み確定データ（id <= 50）のみが参照可能であることを検証
    let visible_rows = conn1.query("SELECT COUNT(*) FROM logs").unwrap();
    assert_eq!(visible_rows[0].get(0), Some(&Value::BigInt(50)));

    let uncommitted_check = conn1.query("SELECT * FROM logs WHERE id = 9999").unwrap();
    assert!(uncommitted_check.is_empty(), "Uncommitted data must not be visible");

    // 7. データベースを一度完全に閉じて再オープンし、永続化されたディスク内容を検証
    drop(conn1);
    drop(conn2);

    let reloaded_conn = Connection::open(&db_path).unwrap();
    let reloaded_count = reloaded_conn.query("SELECT COUNT(*) FROM logs").unwrap();
    assert_eq!(reloaded_count[0].get(0), Some(&Value::BigInt(50)));

    let uncommitted_reload_check = reloaded_conn.query("SELECT * FROM logs WHERE id = 9999").unwrap();
    assert!(uncommitted_reload_check.is_empty(), "Vacuumed file must not contain uncommitted data after reload");
}
