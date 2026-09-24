use h2::{Connection, Value};
use tempfile::tempdir;

#[test]
fn test_advanced_sql_update() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("update_test.db");
    let conn = Connection::open(&db_path).unwrap();

    conn.execute("CREATE TABLE users (id INT PRIMARY KEY, name VARCHAR, age INT, score DOUBLE);").unwrap();
    conn.execute("INSERT INTO users VALUES (1, 'Alice', 20, 85.5);").unwrap();
    conn.execute("INSERT INTO users VALUES (2, 'Bob', 25, 90.0);").unwrap();
    conn.execute("INSERT INTO users VALUES (3, 'Charlie', 30, 75.0);").unwrap();

    // 単一列更新
    let res = conn.execute("UPDATE users SET age = 21 WHERE id = 1;").unwrap();
    assert_eq!(res, 1);

    let rows = conn.query("SELECT age FROM users WHERE id = 1;").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get(0), Some(&Value::Integer(21)));

    // 複数列更新 & 式計算 (age = age + 5)
    let res = conn.execute("UPDATE users SET age = age + 5, score = 95.0 WHERE id = 2;").unwrap();
    assert_eq!(res, 1);

    let rows = conn.query("SELECT age, score FROM users WHERE id = 2;").unwrap();
    assert_eq!(rows[0].get(0), Some(&Value::Integer(30)));
    assert_eq!(rows[0].get(1), Some(&Value::Double(95.0)));

    // 全行一括更新
    let res = conn.execute("UPDATE users SET score = 100.0;").unwrap();
    assert_eq!(res, 3);


    let rows = conn.query("SELECT score FROM users;").unwrap();
    for row in rows {
        assert_eq!(row.get(0), Some(&Value::Double(100.0)));
    }
}

#[test]
fn test_advanced_sql_order_by_limit_offset() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("order_limit_test.db");
    let conn = Connection::open(&db_path).unwrap();

    conn.execute("CREATE TABLE items (id INT, name VARCHAR, price INT);").unwrap();
    conn.execute("INSERT INTO items VALUES (1, 'Apple', 150);").unwrap();
    conn.execute("INSERT INTO items VALUES (2, 'Banana', 100);").unwrap();
    conn.execute("INSERT INTO items VALUES (3, 'Cherry', 300);").unwrap();
    conn.execute("INSERT INTO items VALUES (4, 'Date', 200);").unwrap();
    conn.execute("INSERT INTO items VALUES (5, 'Elderberry', 250);").unwrap();

    // ORDER BY ASC
    let rows = conn.query("SELECT name, price FROM items ORDER BY price ASC;").unwrap();
    assert_eq!(rows.len(), 5);
    assert_eq!(rows[0].get(0), Some(&Value::String("Banana".to_string())));
    assert_eq!(rows[4].get(0), Some(&Value::String("Cherry".to_string())));

    // ORDER BY DESC with LIMIT & OFFSET
    let rows = conn.query("SELECT name, price FROM items ORDER BY price DESC LIMIT 2 OFFSET 1;").unwrap();
    assert_eq!(rows.len(), 2);
    // 全体降順: Cherry(300), Elderberry(250), Date(200), Apple(150), Banana(100)
    // OFFSET 1 -> Elderberry(250), Date(200)
    assert_eq!(rows[0].get(0), Some(&Value::String("Elderberry".to_string())));
    assert_eq!(rows[0].get(1), Some(&Value::Integer(250)));
    assert_eq!(rows[1].get(0), Some(&Value::String("Date".to_string())));
    assert_eq!(rows[1].get(1), Some(&Value::Integer(200)));

    // SELECTに含まれない列でのORDER BY
    let rows = conn.query("SELECT name FROM items ORDER BY price ASC LIMIT 1;").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get(0), Some(&Value::String("Banana".to_string())));

    // エイリアスでのORDER BY
    let rows = conn.query("SELECT name, price * 2 AS double_price FROM items ORDER BY double_price DESC LIMIT 1;").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get(0), Some(&Value::String("Cherry".to_string())));
    assert_eq!(rows[0].get(1), Some(&Value::Integer(600)));
}

#[test]
fn test_advanced_sql_aggregations_and_group_by() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("agg_test.db");
    let conn = Connection::open(&db_path).unwrap();

    conn.execute("CREATE TABLE sales (id INT, dept VARCHAR, amount INT);").unwrap();
    conn.execute("INSERT INTO sales VALUES (1, 'Engineering', 500);").unwrap();
    conn.execute("INSERT INTO sales VALUES (2, 'Engineering', 700);").unwrap();
    conn.execute("INSERT INTO sales VALUES (3, 'Sales', 300);").unwrap();
    conn.execute("INSERT INTO sales VALUES (4, 'Sales', 400);").unwrap();
    conn.execute("INSERT INTO sales VALUES (5, 'Sales', 500);").unwrap();
    conn.execute("INSERT INTO sales VALUES (6, 'HR', 200);").unwrap();

    // テーブル全体に対する集約関数
    let rows = conn.query("SELECT COUNT(*), SUM(amount), AVG(amount), MIN(amount), MAX(amount) FROM sales;").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get(0), Some(&Value::BigInt(6))); // COUNT(*)
    assert_eq!(rows[0].get(1), Some(&Value::Integer(2600))); // SUM(amount)
    // AVG: 2600 / 6 = 433.3333333333333
    if let Some(Value::Double(avg)) = rows[0].get(2) {
        assert!((avg - 433.33333).abs() < 0.01);
    } else {
        panic!("Expected double for AVG");
    }
    assert_eq!(rows[0].get(3), Some(&Value::Integer(200))); // MIN(amount)
    assert_eq!(rows[0].get(4), Some(&Value::Integer(700))); // MAX(amount)

    // GROUP BY + 集約 + ORDER BY
    let rows = conn.query("SELECT dept, COUNT(*), SUM(amount) FROM sales GROUP BY dept ORDER BY dept ASC;").unwrap();
    assert_eq!(rows.len(), 3);
    // Engineering: 2件, 1200
    assert_eq!(rows[0].get(0), Some(&Value::String("Engineering".to_string())));
    assert_eq!(rows[0].get(1), Some(&Value::BigInt(2)));
    assert_eq!(rows[0].get(2), Some(&Value::Integer(1200)));
    // HR: 1件, 200
    assert_eq!(rows[1].get(0), Some(&Value::String("HR".to_string())));
    assert_eq!(rows[1].get(1), Some(&Value::BigInt(1)));
    assert_eq!(rows[1].get(2), Some(&Value::Integer(200)));
    // Sales: 3件, 1200
    assert_eq!(rows[2].get(0), Some(&Value::String("Sales".to_string())));
    assert_eq!(rows[2].get(1), Some(&Value::BigInt(3)));
    assert_eq!(rows[2].get(2), Some(&Value::Integer(1200)));

    // GROUP BY + HAVING フィルタ (COUNT >= 2 の部門のみ)
    let rows = conn.query("SELECT dept, COUNT(*), SUM(amount) FROM sales GROUP BY dept HAVING COUNT(*) >= 2 ORDER BY dept ASC;").unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].get(0), Some(&Value::String("Engineering".to_string())));
    assert_eq!(rows[1].get(0), Some(&Value::String("Sales".to_string())));
}

#[test]
fn test_advanced_sql_joins() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("join_test.db");
    let conn = Connection::open(&db_path).unwrap();

    conn.execute("CREATE TABLE customers (id INT PRIMARY KEY, name VARCHAR);").unwrap();
    conn.execute("CREATE TABLE orders (id INT PRIMARY KEY, cust_id INT, product VARCHAR, amount INT);").unwrap();

    conn.execute("INSERT INTO customers VALUES (1, 'Alice');").unwrap();
    conn.execute("INSERT INTO customers VALUES (2, 'Bob');").unwrap();
    conn.execute("INSERT INTO customers VALUES (3, 'Charlie');").unwrap();

    conn.execute("INSERT INTO orders VALUES (101, 1, 'Laptop', 1200);").unwrap();
    conn.execute("INSERT INTO orders VALUES (102, 1, 'Mouse', 25);").unwrap();
    conn.execute("INSERT INTO orders VALUES (103, 2, 'Keyboard', 75);").unwrap();
    // Charlie (id=3) には注文がない

    // INNER JOIN
    let rows = conn.query("SELECT c.name, o.product, o.amount FROM customers c INNER JOIN orders o ON c.id = o.cust_id ORDER BY o.id ASC;").unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].get(0), Some(&Value::String("Alice".to_string())));
    assert_eq!(rows[0].get(1), Some(&Value::String("Laptop".to_string())));
    assert_eq!(rows[0].get(2), Some(&Value::Integer(1200)));

    assert_eq!(rows[1].get(0), Some(&Value::String("Alice".to_string())));
    assert_eq!(rows[1].get(1), Some(&Value::String("Mouse".to_string())));
    assert_eq!(rows[1].get(2), Some(&Value::Integer(25)));

    assert_eq!(rows[2].get(0), Some(&Value::String("Bob".to_string())));
    assert_eq!(rows[2].get(1), Some(&Value::String("Keyboard".to_string())));
    assert_eq!(rows[2].get(2), Some(&Value::Integer(75)));

    // LEFT OUTER JOIN (CharlieもNULL付きで取得される)
    let rows = conn.query("SELECT c.name, o.product FROM customers c LEFT JOIN orders o ON c.id = o.cust_id ORDER BY c.id ASC, o.id ASC;").unwrap();
    assert_eq!(rows.len(), 4);
    assert_eq!(rows[0].get(0), Some(&Value::String("Alice".to_string())));
    assert_eq!(rows[0].get(1), Some(&Value::String("Laptop".to_string())));

    assert_eq!(rows[1].get(0), Some(&Value::String("Alice".to_string())));
    assert_eq!(rows[1].get(1), Some(&Value::String("Mouse".to_string())));

    assert_eq!(rows[2].get(0), Some(&Value::String("Bob".to_string())));
    assert_eq!(rows[2].get(1), Some(&Value::String("Keyboard".to_string())));

    assert_eq!(rows[3].get(0), Some(&Value::String("Charlie".to_string())));
    assert_eq!(rows[3].get(1), Some(&Value::Null)); // 注文なしのため NULL
}
