use h2::Connection;
use h2_types::Value;

#[test]
fn test_insert_returning() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute("CREATE TABLE products (id INT PRIMARY KEY, name VARCHAR, price DECIMAL(10, 2))").unwrap();

    // RETURNING *
    let rows = conn.query("INSERT INTO products VALUES (1, 'Apple', 1.50) RETURNING *").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get_as::<i32>(0).unwrap(), 1);
    assert_eq!(rows[0].get_as::<String>(1).unwrap(), "Apple");

    // RETURNING specific columns with alias
    let rows = conn.query("INSERT INTO products VALUES (2, 'Banana', 0.80) RETURNING name, price AS cost").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get_as::<String>(0).unwrap(), "Banana");
}

#[test]
fn test_update_returning() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute("CREATE TABLE inventory (id INT PRIMARY KEY, stock INT)").unwrap();
    conn.execute("INSERT INTO inventory VALUES (1, 10), (2, 20), (3, 30)").unwrap();

    // UPDATE RETURNING
    let rows = conn.query("UPDATE inventory SET stock = stock + 5 WHERE id >= 2 RETURNING id, stock").unwrap();
    assert_eq!(rows.len(), 2);
    let mut ids: Vec<i32> = rows.iter().map(|r| r.get_as::<i32>(0).unwrap()).collect();
    ids.sort();
    assert_eq!(ids, vec![2, 3]);

    let mut stocks: Vec<i32> = rows.iter().map(|r| r.get_as::<i32>(1).unwrap()).collect();
    stocks.sort();
    assert_eq!(stocks, vec![25, 35]);
}

#[test]
fn test_delete_returning() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute("CREATE TABLE orders (id INT PRIMARY KEY, amount INT)").unwrap();
    conn.execute("INSERT INTO orders VALUES (101, 500), (102, 1200), (103, 300)").unwrap();

    // DELETE RETURNING *
    let rows = conn.query("DELETE FROM orders WHERE amount < 1000 RETURNING *").unwrap();
    assert_eq!(rows.len(), 2);
    let mut ids: Vec<i32> = rows.iter().map(|r| r.get_as::<i32>(0).unwrap()).collect();
    ids.sort();
    assert_eq!(ids, vec![101, 103]);

    // Remaining row
    let rows = conn.query("SELECT COUNT(*) FROM orders").unwrap();
    assert_eq!(rows[0].get(0), Some(&Value::BigInt(1)));
}
