use h2::{Connection, Value};

#[test]
fn test_truncate_table() {
    let conn = Connection::open_in_memory().unwrap();

    conn.execute("CREATE TABLE logs (id INT PRIMARY KEY, message VARCHAR(100))").unwrap();
    conn.execute("CREATE INDEX idx_logs_msg ON logs (message)").unwrap();

    conn.execute("INSERT INTO logs (id, message) VALUES (1, 'info')").unwrap();
    conn.execute("INSERT INTO logs (id, message) VALUES (2, 'warn')").unwrap();
    conn.execute("INSERT INTO logs (id, message) VALUES (3, 'error')").unwrap();

    let rows = conn.query("SELECT * FROM logs").unwrap();
    assert_eq!(rows.len(), 3);

    // TRUNCATE TABLE
    conn.execute("TRUNCATE TABLE logs").unwrap();

    let rows = conn.query("SELECT * FROM logs").unwrap();
    assert_eq!(rows.len(), 0);

    // 再挿入できること
    conn.execute("INSERT INTO logs (id, message) VALUES (1, 'new_info')").unwrap();
    let rows = conn.query("SELECT * FROM logs WHERE message = 'new_info'").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get_as::<i32>(0).unwrap(), 1);
    assert_eq!(rows[0].get_as::<String>(1).unwrap(), "new_info");
}

#[test]
fn test_alter_table_rename_to() {
    let conn = Connection::open_in_memory().unwrap();

    conn.execute("CREATE TABLE customers (id INT PRIMARY KEY, name VARCHAR(50))").unwrap();
    conn.execute("CREATE INDEX idx_cust_name ON customers (name)").unwrap();

    conn.execute("INSERT INTO customers (id, name) VALUES (10, 'Alice')").unwrap();
    conn.execute("INSERT INTO customers (id, name) VALUES (20, 'Bob')").unwrap();

    // ALTER TABLE RENAME TO
    conn.execute("ALTER TABLE customers RENAME TO clients").unwrap();

    // 旧テーブル名はアクセス不可
    assert!(conn.query("SELECT * FROM customers").is_err());

    // 新テーブル名で取得可能
    let rows = conn.query("SELECT id, name FROM clients").unwrap();
    assert_eq!(rows.len(), 2);

    // インデックス経由での検索も動作
    let rows = conn.query("SELECT id, name FROM clients WHERE name = 'Alice'").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get_as::<i32>(0).unwrap(), 10);

    // 新テーブル名への新規挿入
    conn.execute("INSERT INTO clients (id, name) VALUES (30, 'Charlie')").unwrap();
    let rows = conn.query("SELECT * FROM clients").unwrap();
    assert_eq!(rows.len(), 3);
}

#[test]
fn test_alter_table_add_column() {
    let conn = Connection::open_in_memory().unwrap();

    conn.execute("CREATE TABLE employees (id INT PRIMARY KEY, name VARCHAR(50))").unwrap();
    conn.execute("INSERT INTO employees (id, name) VALUES (1, 'Taro')").unwrap();

    // 列追加
    conn.execute("ALTER TABLE employees ADD COLUMN department VARCHAR(50)").unwrap();

    // 既存行は NULL が入っていること
    let rows = conn.query("SELECT id, name, department FROM employees").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get_as::<i32>(0).unwrap(), 1);
    assert_eq!(rows[0].get_as::<String>(1).unwrap(), "Taro");
    assert_eq!(rows[0].values[2], Value::Null);

    // 新規行は新カラム付きで挿入可能
    conn.execute("INSERT INTO employees (id, name, department) VALUES (2, 'Hanako', 'Engineering')").unwrap();
    let rows = conn.query("SELECT id, name, department FROM employees WHERE department = 'Engineering'").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get_as::<i32>(0).unwrap(), 2);
    assert_eq!(rows[0].get_as::<String>(2).unwrap(), "Engineering");
}

#[test]
fn test_alter_table_drop_column() {
    let conn = Connection::open_in_memory().unwrap();

    conn.execute("CREATE TABLE users (id INT PRIMARY KEY, name VARCHAR(50), temp_token VARCHAR(100))").unwrap();
    conn.execute("INSERT INTO users (id, name, temp_token) VALUES (1, 'Alice', 'tok_123')").unwrap();
    conn.execute("INSERT INTO users (id, name, temp_token) VALUES (2, 'Bob', 'tok_456')").unwrap();

    // 列削除
    conn.execute("ALTER TABLE users DROP COLUMN temp_token").unwrap();

    // temp_token が無くなり、id と name のみ
    let rows = conn.query("SELECT * FROM users").unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].values.len(), 2);
    assert_eq!(rows[0].get_as::<i32>(0).unwrap(), 1);
    assert_eq!(rows[0].get_as::<String>(1).unwrap(), "Alice");

    // 新規行も2カラムで挿入可能
    conn.execute("INSERT INTO users (id, name) VALUES (3, 'Charlie')").unwrap();
    let rows = conn.query("SELECT id, name FROM users").unwrap();
    assert_eq!(rows.len(), 3);
}

#[test]
fn test_explain_statement() {
    let conn = Connection::open_in_memory().unwrap();

    conn.execute("CREATE TABLE users (id INT PRIMARY KEY, name VARCHAR(50))").unwrap();
    conn.execute("CREATE TABLE orders (order_id INT PRIMARY KEY, user_id INT, amount INT)").unwrap();
    conn.execute("CREATE INDEX idx_users_name ON users (name)").unwrap();

    // EXPLAIN SELECT (IndexScan)
    let rows = conn.query("EXPLAIN SELECT * FROM users WHERE name = 'Alice'").unwrap();
    assert_eq!(rows.len(), 1);
    let plan = rows[0].get_as::<String>(0).unwrap();
    assert!(plan.contains("IndexScan"));

    // The primary-key predicate uses an index scan as well.
    let rows = conn.query("EXPLAIN SELECT * FROM users WHERE id = 1").unwrap();
    let plan = rows[0].get_as::<String>(0).unwrap();
    assert!(plan.contains("IndexScan: users on index pk_"));

    // EXPLAIN SELECT with JOIN
    let rows = conn.query("EXPLAIN SELECT * FROM users JOIN orders ON users.id = orders.user_id").unwrap();
    let plan = rows[0].get_as::<String>(0).unwrap();
    assert!(plan.contains("NestedLoopJoin: orders"));
}

#[test]
fn test_show_tables_and_columns() {
    let conn = Connection::open_in_memory().unwrap();

    conn.execute("CREATE TABLE authors (id INT PRIMARY KEY, name VARCHAR(50) NOT NULL)").unwrap();
    conn.execute("CREATE TABLE books (book_id INT PRIMARY KEY, title VARCHAR(100), author_id INT)").unwrap();

    // SHOW TABLES
    let rows = conn.query("SHOW TABLES").unwrap();
    assert_eq!(rows.len(), 2);
    let table_names: Vec<String> = rows.iter().map(|r| r.get_as::<String>(0).unwrap()).collect();
    assert!(table_names.contains(&"authors".to_string()));
    assert!(table_names.contains(&"books".to_string()));

    // SHOW COLUMNS FROM authors
    let rows = conn.query("SHOW COLUMNS FROM authors").unwrap();
    assert_eq!(rows.len(), 2);
    // Field, Type, Null, Key
    assert_eq!(rows[0].get_as::<String>(0).unwrap(), "id");
    assert_eq!(rows[0].get_as::<String>(3).unwrap(), "PRI");

    assert_eq!(rows[1].get_as::<String>(0).unwrap(), "name");
    assert_eq!(rows[1].get_as::<String>(2).unwrap(), "NO"); // NOT NULL
}
