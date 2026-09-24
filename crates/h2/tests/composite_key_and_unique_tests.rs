use h2::Connection;

#[test]
fn test_composite_primary_key() {
    let conn = Connection::open_in_memory().unwrap();

    conn.execute(
        "CREATE TABLE order_items (
            order_id INT,
            item_id INT,
            quantity INT,
            PRIMARY KEY (order_id, item_id)
        )"
    ).unwrap();

    // 正常挿入
    conn.execute("INSERT INTO order_items VALUES (101, 1, 2)").unwrap();
    conn.execute("INSERT INTO order_items VALUES (101, 2, 5)").unwrap();
    conn.execute("INSERT INTO order_items VALUES (102, 1, 1)").unwrap();

    let rows = conn.query("SELECT * FROM order_items ORDER BY order_id, item_id").unwrap();
    assert_eq!(rows.len(), 3);

    // 重複キー挿入の拒絶
    let err = conn.execute("INSERT INTO order_items VALUES (101, 1, 10)").unwrap_err();
    assert!(err.to_string().contains("Unique constraint violation"), "error was: {}", err);

    // 更新による重複の拒絶
    let update_err = conn.execute("UPDATE order_items SET item_id = 1 WHERE order_id = 101 AND item_id = 2").unwrap_err();
    assert!(update_err.to_string().contains("Unique constraint violation"), "error was: {}", update_err);

    // 正常な更新
    conn.execute("UPDATE order_items SET quantity = 20 WHERE order_id = 101 AND item_id = 1").unwrap();
    let rows = conn.query("SELECT quantity FROM order_items WHERE order_id = 101 AND item_id = 1").unwrap();
    assert_eq!(rows[0].get_as::<i32>(0).unwrap(), 20);

    // 削除後の再挿入
    conn.execute("DELETE FROM order_items WHERE order_id = 101 AND item_id = 1").unwrap();
    conn.execute("INSERT INTO order_items VALUES (101, 1, 99)").unwrap();
    let rows = conn.query("SELECT quantity FROM order_items WHERE order_id = 101 AND item_id = 1").unwrap();
    assert_eq!(rows[0].get_as::<i32>(0).unwrap(), 99);
}

#[test]
fn test_composite_unique_constraint() {
    let conn = Connection::open_in_memory().unwrap();

    conn.execute(
        "CREATE TABLE accounts (
            id INT PRIMARY KEY,
            tenant_id INT,
            email VARCHAR(100),
            CONSTRAINT u_tenant_email UNIQUE (tenant_id, email)
        )"
    ).unwrap();

    conn.execute("INSERT INTO accounts VALUES (1, 10, 'alice@example.com')").unwrap();
    conn.execute("INSERT INTO accounts VALUES (2, 20, 'alice@example.com')").unwrap(); // tenant_id が異なるためOK

    let rows = conn.query("SELECT * FROM accounts").unwrap();
    assert_eq!(rows.len(), 2);

    // 重複挿入の拒絶 (10, 'alice@example.com')
    let err = conn.execute("INSERT INTO accounts VALUES (3, 10, 'alice@example.com')").unwrap_err();
    assert!(err.to_string().contains("Unique constraint violation"), "error was: {}", err);
}

#[test]
fn test_column_level_unique_constraint() {
    let conn = Connection::open_in_memory().unwrap();

    conn.execute(
        "CREATE TABLE users (
            id INT PRIMARY KEY,
            username VARCHAR(50) UNIQUE
        )"
    ).unwrap();

    conn.execute("INSERT INTO users VALUES (1, 'admin')").unwrap();
    let err = conn.execute("INSERT INTO users VALUES (2, 'admin')").unwrap_err();
    assert!(err.to_string().contains("Unique constraint violation"), "error was: {}", err);
}
