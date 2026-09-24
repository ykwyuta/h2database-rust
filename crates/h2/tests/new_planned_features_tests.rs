use std::str::FromStr;
use rust_decimal::Decimal;
use uuid::Uuid;
use h2::{params, AsyncConnection, Connection, Value};

#[test]
fn test_drop_table_and_drop_index() {
    let conn = Connection::open_in_memory().unwrap();

    conn.execute(
        "CREATE TABLE products (id INT PRIMARY KEY, title VARCHAR(100), price DECIMAL(10, 2))"
    ).unwrap();

    conn.execute("CREATE INDEX idx_products_title ON products (title)").unwrap();

    conn.execute_params(
        "INSERT INTO products (id, title, price) VALUES (?, ?, ?)",
        &params![1, "Mechanical Keyboard", Decimal::from_str("120.00").unwrap()],
    ).unwrap();

    let rows = conn.query("SELECT * FROM products WHERE title = 'Mechanical Keyboard'").unwrap();
    assert_eq!(rows.len(), 1);

    // インデックス削除
    conn.execute("DROP INDEX idx_products_title").unwrap();

    // 存在しないインデックス削除（IF EXISTS）
    conn.execute("DROP INDEX IF EXISTS idx_non_existent").unwrap();

    // インデックス削除後もデータはそのまま検索可能
    let rows = conn.query("SELECT * FROM products").unwrap();
    assert_eq!(rows.len(), 1);

    // テーブル削除
    conn.execute("DROP TABLE products").unwrap();

    // 削除後のテーブルにクエリするとエラー
    assert!(conn.query("SELECT * FROM products").is_err());

    // 存在しないテーブル削除（IF EXISTS）
    conn.execute("DROP TABLE IF EXISTS products").unwrap();

    // 削除したテーブル名で再作成可能
    conn.execute("CREATE TABLE products (code INT, note VARCHAR)").unwrap();
    conn.execute("INSERT INTO products VALUES (42, 'New Products')").unwrap();
    let rows = conn.query("SELECT code, note FROM products").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get_as::<i32>(0).unwrap(), 42);
    assert_eq!(rows[0].get_as::<String>(1).unwrap(), "New Products");
}

#[test]
fn test_from_sql_and_row_get_as() {
    let conn = Connection::open_in_memory().unwrap();

    conn.execute(
        "CREATE TABLE types_demo (
            id INT PRIMARY KEY,
            big_val BIGINT,
            price DECIMAL(10, 2),
            score DOUBLE,
            is_active BOOLEAN,
            name VARCHAR(50),
            tag UUID,
            optional_note VARCHAR(100)
        )"
    ).unwrap();

    let uid = Uuid::new_v4();
    conn.execute_params(
        "INSERT INTO types_demo VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        &[
            Value::Integer(101),
            Value::BigInt(9876543210),
            Value::Decimal(Decimal::from_str("99.95").unwrap()),
            Value::Double(3.1415),
            Value::Boolean(true),
            Value::String("Rustacean".to_string()),
            Value::Uuid(uid),
            Value::Null,
        ],
    ).unwrap();

    let rows = conn.query("SELECT * FROM types_demo WHERE id = 101").unwrap();
    assert_eq!(rows.len(), 1);
    let row = &rows[0];

    // 型安全な get_as
    let id: i32 = row.get_as(0).unwrap();
    assert_eq!(id, 101);

    let big_val: i64 = row.get_as(1).unwrap();
    assert_eq!(big_val, 9876543210);

    let price: Decimal = row.get_as(2).unwrap();
    assert_eq!(price, Decimal::from_str("99.95").unwrap());

    let score: f64 = row.get_as(3).unwrap();
    assert!((score - 3.1415).abs() < 1e-4);

    let is_active: bool = row.get_as(4).unwrap();
    assert!(is_active);

    let name: String = row.get_as(5).unwrap();
    assert_eq!(name, "Rustacean");

    let tag: Uuid = row.get_as(6).unwrap();
    assert_eq!(tag, uid);

    // Option<T> の型安全な取得（NULL列）
    let note: Option<String> = row.get_as(7).unwrap();
    assert_eq!(note, None);

    // 範囲外のインデックス取得はエラー
    assert!(row.get_as::<i32>(99).is_err());
}

#[test]
fn test_explicit_transaction_sql() {
    let conn = Connection::open_in_memory().unwrap();

    conn.execute("CREATE TABLE wallets (id INT PRIMARY KEY, balance INT)").unwrap();

    // 1. ロールバックテスト
    conn.execute("BEGIN").unwrap();
    assert!(conn.in_transaction());
    conn.execute("INSERT INTO wallets VALUES (1, 500)").unwrap();
    conn.execute("ROLLBACK").unwrap();
    assert!(!conn.in_transaction());

    let rows = conn.query("SELECT * FROM wallets").unwrap();
    assert_eq!(rows.len(), 0);

    // 2. コミットテスト
    conn.execute("START TRANSACTION;").unwrap();
    assert!(conn.in_transaction());
    conn.execute("INSERT INTO wallets VALUES (1, 500)").unwrap();
    conn.execute("INSERT INTO wallets VALUES (2, 300)").unwrap();
    conn.execute("COMMIT;").unwrap();
    assert!(!conn.in_transaction());

    let rows = conn.query("SELECT * FROM wallets ORDER BY id").unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].get_as::<i32>(1).unwrap(), 500);
    assert_eq!(rows[1].get_as::<i32>(1).unwrap(), 300);

    // 3. 多重BEGINでエラー
    conn.execute("BEGIN").unwrap();
    assert!(conn.execute("BEGIN").is_err());
    conn.execute("ROLLBACK").unwrap();

    // 4. トランザクション未開始でのCOMMITでエラー
    assert!(conn.execute("COMMIT").is_err());
}

#[tokio::test]
async fn test_async_connection_and_transaction() {
    let conn = AsyncConnection::open_in_memory().await.unwrap();

    conn.execute("CREATE TABLE tasks (id INT PRIMARY KEY, title VARCHAR, done BOOLEAN)").await.unwrap();

    // 非同期 INSERT & SELECT
    conn.execute_params(
        "INSERT INTO tasks VALUES (?, ?, ?)",
        &params![1, "Implement async API", true],
    ).await.unwrap();

    let rows = conn.query("SELECT id, title, done FROM tasks WHERE id = 1").await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get_as::<i32>(0).unwrap(), 1);
    assert_eq!(rows[0].get_as::<String>(1).unwrap(), "Implement async API");
    assert_eq!(rows[0].get_as::<bool>(2).unwrap(), true);

    // 非同期トランザクション（Rollback）
    let tx = conn.transaction().await.unwrap();
    tx.execute("INSERT INTO tasks VALUES (2, 'Cancelled Task', false)").await.unwrap();
    tx.rollback().await.unwrap();

    let rows = conn.query("SELECT * FROM tasks").await.unwrap();
    assert_eq!(rows.len(), 1);

    // 非同期トランザクション（Commit）
    let tx = conn.transaction().await.unwrap();
    tx.execute("INSERT INTO tasks VALUES (2, 'Completed Task', true)").await.unwrap();
    tx.commit().await.unwrap();

    let rows = conn.query("SELECT * FROM tasks ORDER BY id").await.unwrap();
    assert_eq!(rows.len(), 2);

    // Tokio の並行タスクからのアクセス
    let mut handles = vec![];
    for i in 10..15 {
        let c = conn.clone();
        handles.push(tokio::spawn(async move {
            c.execute_params(
                "INSERT INTO tasks VALUES (?, ?, ?)",
                &params![i, format!("Concurrent task {}", i), false],
            ).await.unwrap();
        }));
    }

    for h in handles {
        h.await.unwrap();
    }

    let all_tasks = conn.query("SELECT count(*) FROM tasks").await.unwrap();
    assert_eq!(all_tasks[0].get_as::<i64>(0).unwrap(), 7);
}
