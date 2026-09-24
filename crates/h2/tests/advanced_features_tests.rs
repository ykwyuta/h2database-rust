use h2::{params, Connection, Value};
use tempfile::tempdir;

#[test]
fn test_secondary_and_unique_index() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("index_test.db");
    let conn = Connection::open(&db_path).unwrap();

    conn.execute("CREATE TABLE users (id INT PRIMARY KEY, email VARCHAR, age INT);").unwrap();
    conn.execute("INSERT INTO users VALUES (1, 'alice@example.com', 25);").unwrap();
    conn.execute("INSERT INTO users VALUES (2, 'bob@example.com', 30);").unwrap();
    conn.execute("INSERT INTO users VALUES (3, 'charlie@example.com', 35);").unwrap();

    // 通常のセカンダリインデックス作成
    conn.execute("CREATE INDEX idx_users_age ON users (age);").unwrap();

    // 一意インデックス作成
    conn.execute("CREATE UNIQUE INDEX idx_users_email ON users (email);").unwrap();

    // IndexScan による検索 (WHERE age = 30)
    let rows = conn.query("SELECT id, email FROM users WHERE age = 30;").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get(0), Some(&Value::Integer(2)));
    assert_eq!(rows[0].get(1), Some(&Value::String("bob@example.com".to_string())));

    // 一意制約違反のテスト (既存の email 'alice@example.com' を重複挿入)
    let duplicate_insert = conn.execute("INSERT INTO users VALUES (4, 'alice@example.com', 28);");
    assert!(duplicate_insert.is_err(), "Expected unique constraint violation on insert");

    // UPDATE でのインデックス同期
    conn.execute("UPDATE users SET age = 40 WHERE id = 1;").unwrap();
    let rows_old_age = conn.query("SELECT id FROM users WHERE age = 25;").unwrap();
    assert_eq!(rows_old_age.len(), 0);
    let rows_new_age = conn.query("SELECT id FROM users WHERE age = 40;").unwrap();
    assert_eq!(rows_new_age.len(), 1);
    assert_eq!(rows_new_age[0].get(0), Some(&Value::Integer(1)));

    // UPDATE による一意制約違反
    let duplicate_update = conn.execute("UPDATE users SET email = 'bob@example.com' WHERE id = 1;");
    assert!(duplicate_update.is_err(), "Expected unique constraint violation on update");

    // DELETE でのインデックス同期
    conn.execute("DELETE FROM users WHERE id = 2;").unwrap();
    let rows_deleted = conn.query("SELECT id FROM users WHERE age = 30;").unwrap();
    assert_eq!(rows_deleted.len(), 0);
}

#[test]
fn test_jsonb_and_json_operators() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("json_test.db");
    let conn = Connection::open(&db_path).unwrap();

    conn.execute("CREATE TABLE docs (id INT PRIMARY KEY, metadata JSON);").unwrap();

    // JSON データの挿入
    conn.execute("INSERT INTO docs VALUES (1, '{\"user\": {\"name\": \"Alice\", \"role\": \"admin\"}, \"tags\": [\"rust\", \"db\"]}');").unwrap();
    conn.execute("INSERT INTO docs VALUES (2, '{\"user\": {\"name\": \"Bob\", \"role\": \"guest\"}, \"tags\": [\"sql\"]}');").unwrap();

    // JSON_EXTRACT 関数での抽出
    let rows = conn.query("SELECT JSON_EXTRACT(metadata, '$.user.name') FROM docs WHERE id = 1;").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get(0), Some(&Value::String("Alice".to_string())));

    // -> 演算子 (JSONオブジェクト/配列抽出)
    let rows = conn.query("SELECT metadata -> 'user' FROM docs WHERE id = 1;").unwrap();
    assert_eq!(rows.len(), 1);
    assert!(matches!(rows[0].get(0), Some(Value::Json(_))));

    // ->> 演算子 (テキスト抽出)
    let rows = conn.query("SELECT metadata ->> '$.user.role' FROM docs WHERE id = 2;").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get(0), Some(&Value::String("guest".to_string())));
}

#[test]
fn test_parameterized_queries_and_params_macro() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("params_test.db");
    let conn = Connection::open(&db_path).unwrap();

    conn.execute("CREATE TABLE products (id INT PRIMARY KEY, name VARCHAR, price INT);").unwrap();

    // params! マクロと execute_params ($1, $2, $3)
    let count = conn.execute_params(
        "INSERT INTO products VALUES ($1, $2, $3);",
        &params![101, "Mechanical Keyboard", 150],
    ).unwrap();
    assert_eq!(count, 1);

    conn.execute_params(
        "INSERT INTO products VALUES (?1, ?2, ?3);",
        &params![102, "Gaming Mouse", 75],
    ).unwrap();

    // query_params
    let rows = conn.query_params(
        "SELECT name, price FROM products WHERE price > $1 ORDER BY price DESC;",
        &params![100],
    ).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get(0), Some(&Value::String("Mechanical Keyboard".to_string())));
    assert_eq!(rows[0].get(1), Some(&Value::Integer(150)));

    // トランザクション内でのパラメータ付きクエリ
    let tx = conn.transaction().unwrap();
    let updated = tx.execute_params(
        "UPDATE products SET price = ?1 WHERE id = ?2;",
        &params![80, 102],
    ).unwrap();
    assert_eq!(updated, 1);
    tx.commit().unwrap();

    let rows_after = conn.query_params("SELECT price FROM products WHERE id = $1;", &params![102]).unwrap();
    assert_eq!(rows_after[0].get(0), Some(&Value::Integer(80)));
}

#[test]
fn test_information_schema_system_views() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("info_schema_test.db");
    let conn = Connection::open(&db_path).unwrap();

    conn.execute("CREATE TABLE authors (id INT PRIMARY KEY, name VARCHAR);").unwrap();
    conn.execute("CREATE TABLE books (id INT PRIMARY KEY, title VARCHAR, author_id INT);").unwrap();

    // information_schema.tables のクエリ
    let tables = conn.query("SELECT table_name, columns_count FROM information_schema.tables ORDER BY table_name ASC;").unwrap();
    assert_eq!(tables.len(), 2);
    assert_eq!(tables[0].get(0), Some(&Value::String("authors".to_string())));
    assert_eq!(tables[0].get(1), Some(&Value::Integer(2)));
    assert_eq!(tables[1].get(0), Some(&Value::String("books".to_string())));
    assert_eq!(tables[1].get(1), Some(&Value::Integer(3)));

    // information_schema.columns のクエリ (WHERE フィルタ付き)
    let cols = conn.query("SELECT column_name, data_type FROM information_schema.columns WHERE table_name = 'authors' ORDER BY column_name ASC;").unwrap();
    assert_eq!(cols.len(), 2);
    assert_eq!(cols[0].get(0), Some(&Value::String("id".to_string())));
    assert_eq!(cols[1].get(0), Some(&Value::String("name".to_string())));
}
