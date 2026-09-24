use h2::Connection;

#[test]
fn test_select_without_from() {
    let conn = Connection::open_in_memory().unwrap();

    // 1. 四則演算とリテラル
    let rows = conn.query("SELECT 1 + 1 AS res").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get_as::<i32>(0).unwrap(), 2);

    // 2. 複数カラム & 文字列
    let rows = conn.query("SELECT 'Rust' AS lang, 42 AS answer").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get_as::<String>(0).unwrap(), "Rust");
    assert_eq!(rows[0].get_as::<i32>(1).unwrap(), 42);

    // 3. スカラ関数 (UPPER, ABS, CONCAT)
    let rows = conn.query("SELECT UPPER('hello') AS up, ABS(-99) AS val, CONCAT('a', 'b', 'c') AS merged").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get_as::<String>(0).unwrap(), "HELLO");
    assert_eq!(rows[0].get_as::<i32>(1).unwrap(), 99);
    assert_eq!(rows[0].get_as::<String>(2).unwrap(), "abc");

    // 4. CASE WHEN
    let rows = conn.query("SELECT CASE WHEN 10 > 5 THEN 'high' ELSE 'low' END AS grade").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get_as::<String>(0).unwrap(), "high");

    // 5. WHERE 句付きの FROM なし SELECT
    let rows = conn.query("SELECT 100 AS num WHERE 1 = 1").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get_as::<i32>(0).unwrap(), 100);

    let rows = conn.query("SELECT 100 AS num WHERE 1 = 2").unwrap();
    assert_eq!(rows.len(), 0);
}

#[test]
fn test_case_when_and_scalar_functions() {
    let conn = Connection::open_in_memory().unwrap();

    conn.execute("CREATE TABLE students (id INT PRIMARY KEY, name VARCHAR(50), score INT, nickname VARCHAR(50))").unwrap();
    conn.execute("INSERT INTO students VALUES (1, 'Alice', 95, 'Ali')").unwrap();
    conn.execute("INSERT INTO students VALUES (2, 'Bob', 75, NULL)").unwrap();
    conn.execute("INSERT INTO students VALUES (3, 'Charlie', 50, NULL)").unwrap();

    // CASE WHEN のテスト
    let rows = conn.query("SELECT name, CASE WHEN score >= 90 THEN 'A' WHEN score >= 70 THEN 'B' ELSE 'C' END AS rank FROM students ORDER BY id").unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].get_as::<String>(1).unwrap(), "A");
    assert_eq!(rows[1].get_as::<String>(1).unwrap(), "B");
    assert_eq!(rows[2].get_as::<String>(1).unwrap(), "C");

    // COALESCE, LOWER, LENGTH のテスト
    let rows = conn.query("SELECT LOWER(name), COALESCE(nickname, name), LENGTH(name) FROM students WHERE id = 2").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get_as::<String>(0).unwrap(), "bob");
    assert_eq!(rows[0].get_as::<String>(1).unwrap(), "Bob"); // nickname が NULL なので name を採用
    assert_eq!(rows[0].get_as::<i32>(2).unwrap(), 3);
}

#[test]
fn test_in_and_between_expressions() {
    let conn = Connection::open_in_memory().unwrap();

    conn.execute("CREATE TABLE items (id INT PRIMARY KEY, price INT)").unwrap();
    conn.execute("INSERT INTO items VALUES (1, 100), (2, 200), (3, 300), (4, 400), (5, 500)").unwrap();

    // IN 式
    let rows = conn.query("SELECT id FROM items WHERE id IN (1, 3, 5) ORDER BY id").unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].get_as::<i32>(0).unwrap(), 1);
    assert_eq!(rows[1].get_as::<i32>(0).unwrap(), 3);
    assert_eq!(rows[2].get_as::<i32>(0).unwrap(), 5);

    // NOT IN 式
    let rows = conn.query("SELECT id FROM items WHERE id NOT IN (1, 2, 3) ORDER BY id").unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].get_as::<i32>(0).unwrap(), 4);
    assert_eq!(rows[1].get_as::<i32>(0).unwrap(), 5);

    // BETWEEN 式
    let rows = conn.query("SELECT id FROM items WHERE price BETWEEN 200 AND 400 ORDER BY id").unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].get_as::<i32>(0).unwrap(), 2);
    assert_eq!(rows[1].get_as::<i32>(0).unwrap(), 3);
    assert_eq!(rows[2].get_as::<i32>(0).unwrap(), 4);
}

#[test]
fn test_derived_table_subqueries() {
    let conn = Connection::open_in_memory().unwrap();

    conn.execute("CREATE TABLE orders (order_id INT PRIMARY KEY, user_id INT, amount INT)").unwrap();
    conn.execute("INSERT INTO orders VALUES (1, 10, 100), (2, 10, 200), (3, 20, 50), (4, 30, 500)").unwrap();

    // FROM 句でのサブクエリ（派生テーブル）
    let rows = conn.query("SELECT sub.user_id, sub.total FROM (SELECT user_id, SUM(amount) AS total FROM orders GROUP BY user_id) AS sub WHERE sub.total >= 300 ORDER BY sub.total DESC").unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].get_as::<i32>(0).unwrap(), 30);
    assert_eq!(rows[0].get_as::<i64>(1).unwrap(), 500);
    assert_eq!(rows[1].get_as::<i32>(0).unwrap(), 10);
    assert_eq!(rows[1].get_as::<i64>(1).unwrap(), 300);

    // JOIN での派生テーブル
    conn.execute("CREATE TABLE users (id INT PRIMARY KEY, name VARCHAR(50))").unwrap();
    conn.execute("INSERT INTO users VALUES (10, 'Alice'), (20, 'Bob'), (30, 'Charlie')").unwrap();

    let rows = conn.query("SELECT users.name, sub.total FROM users JOIN (SELECT user_id, SUM(amount) AS total FROM orders GROUP BY user_id) AS sub ON users.id = sub.user_id ORDER BY users.id").unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].get_as::<String>(0).unwrap(), "Alice");
    assert_eq!(rows[0].get_as::<i64>(1).unwrap(), 300);
    assert_eq!(rows[1].get_as::<String>(0).unwrap(), "Bob");
    assert_eq!(rows[1].get_as::<i64>(1).unwrap(), 50);
}

#[test]
fn test_in_and_exists_subqueries() {
    let conn = Connection::open_in_memory().unwrap();

    conn.execute("CREATE TABLE users (id INT PRIMARY KEY, name VARCHAR(50))").unwrap();
    conn.execute("CREATE TABLE orders (order_id INT PRIMARY KEY, user_id INT, amount INT)").unwrap();

    conn.execute("INSERT INTO users VALUES (1, 'Alice'), (2, 'Bob'), (3, 'Charlie')").unwrap();
    conn.execute("INSERT INTO orders VALUES (10, 1, 300), (20, 2, 50)").unwrap();

    // WHERE IN (SELECT ...) サブクエリ
    let rows = conn.query("SELECT name FROM users WHERE id IN (SELECT user_id FROM orders WHERE amount >= 100)").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get_as::<String>(0).unwrap(), "Alice");

    // WHERE EXISTS (SELECT ...) サブクエリ
    let rows = conn.query("SELECT name FROM users WHERE EXISTS (SELECT 1 FROM orders WHERE amount > 200)").unwrap();
    assert_eq!(rows.len(), 3); // 条件を満たす order が存在するので全ユーザーが返る

    let rows = conn.query("SELECT name FROM users WHERE EXISTS (SELECT 1 FROM orders WHERE amount > 1000)").unwrap();
    assert_eq!(rows.len(), 0); // 存在しないので 0 行

    // スカラサブクエリ (SELECT (SELECT ...))
    let rows = conn.query("SELECT name, (SELECT MAX(amount) FROM orders) AS max_amt FROM users WHERE id = 1").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get_as::<String>(0).unwrap(), "Alice");
    assert_eq!(rows[0].get_as::<i32>(1).unwrap(), 300);
}

#[test]
fn test_distinct_and_union() {
    let conn = Connection::open_in_memory().unwrap();

    conn.execute("CREATE TABLE staff (id INT PRIMARY KEY, dept VARCHAR(50))").unwrap();
    conn.execute("INSERT INTO staff VALUES (1, 'Sales'), (2, 'Dev'), (3, 'Sales'), (4, 'Dev'), (5, 'HR')").unwrap();

    // DISTINCT
    let rows = conn.query("SELECT DISTINCT dept FROM staff ORDER BY dept").unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].get_as::<String>(0).unwrap(), "Dev");
    assert_eq!(rows[1].get_as::<String>(0).unwrap(), "HR");
    assert_eq!(rows[2].get_as::<String>(0).unwrap(), "Sales");

    // UNION (重複排除)
    conn.execute("CREATE TABLE group_a (name VARCHAR(50))").unwrap();
    conn.execute("CREATE TABLE group_b (name VARCHAR(50))").unwrap();
    conn.execute("INSERT INTO group_a VALUES ('Alice'), ('Bob')").unwrap();
    conn.execute("INSERT INTO group_b VALUES ('Bob'), ('Charlie')").unwrap();

    let rows = conn.query("SELECT name FROM group_a UNION SELECT name FROM group_b ORDER BY name").unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].get_as::<String>(0).unwrap(), "Alice");
    assert_eq!(rows[1].get_as::<String>(0).unwrap(), "Bob");
    assert_eq!(rows[2].get_as::<String>(0).unwrap(), "Charlie");

    // UNION ALL (重複保持)
    let rows = conn.query("SELECT name FROM group_a UNION ALL SELECT name FROM group_b").unwrap();
    assert_eq!(rows.len(), 4);
}
