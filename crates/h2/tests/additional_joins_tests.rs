use h2::Connection;
use h2_types::Value;

#[test]
fn test_right_and_full_outer_join() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute("CREATE TABLE t1 (id INT PRIMARY KEY, val1 VARCHAR)").unwrap();
    conn.execute("CREATE TABLE t2 (id INT PRIMARY KEY, val2 VARCHAR)").unwrap();

    conn.execute("INSERT INTO t1 VALUES (1, 'A'), (2, 'B')").unwrap();
    conn.execute("INSERT INTO t2 VALUES (2, 'X'), (3, 'Y')").unwrap();

    // RIGHT OUTER JOIN
    let rows = conn.query("SELECT t1.id, t1.val1, t2.id, t2.val2 FROM t1 RIGHT JOIN t2 ON t1.id = t2.id ORDER BY t2.id").unwrap();
    assert_eq!(rows.len(), 2);
    // id=2 matches
    assert_eq!(rows[0].get_as::<i32>(0).unwrap(), 2);
    assert_eq!(rows[0].get_as::<String>(1).unwrap(), "B");
    assert_eq!(rows[0].get_as::<i32>(2).unwrap(), 2);
    assert_eq!(rows[0].get_as::<String>(3).unwrap(), "X");
    // id=3 only in t2 -> t1.* is NULL
    assert_eq!(rows[1].get(0), Some(&Value::Null));
    assert_eq!(rows[1].get(1), Some(&Value::Null));
    assert_eq!(rows[1].get_as::<i32>(2).unwrap(), 3);
    assert_eq!(rows[1].get_as::<String>(3).unwrap(), "Y");

    // FULL OUTER JOIN
    let rows_full = conn.query("SELECT t1.id, t1.val1, t2.id, t2.val2 FROM t1 FULL OUTER JOIN t2 ON t1.id = t2.id ORDER BY COALESCE(t1.id, t2.id)").unwrap();
    assert_eq!(rows_full.len(), 3);
    // id=1 only in t1
    assert_eq!(rows_full[0].get_as::<i32>(0).unwrap(), 1);
    assert_eq!(rows_full[0].get(2), Some(&Value::Null));
    // id=2 both
    assert_eq!(rows_full[1].get_as::<i32>(0).unwrap(), 2);
    assert_eq!(rows_full[1].get_as::<i32>(2).unwrap(), 2);
    // id=3 only in t2
    assert_eq!(rows_full[2].get(0), Some(&Value::Null));
    assert_eq!(rows_full[2].get_as::<i32>(2).unwrap(), 3);
}

#[test]
fn test_cross_join() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute("CREATE TABLE colors (name VARCHAR)").unwrap();
    conn.execute("CREATE TABLE sizes (size VARCHAR)").unwrap();

    conn.execute("INSERT INTO colors VALUES ('Red'), ('Blue')").unwrap();
    conn.execute("INSERT INTO sizes VALUES ('S'), ('M'), ('L')").unwrap();

    let rows = conn.query("SELECT colors.name, sizes.size FROM colors CROSS JOIN sizes ORDER BY colors.name, sizes.size").unwrap();
    assert_eq!(rows.len(), 6); // 2 * 3
}

#[test]
fn test_join_using() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute("CREATE TABLE depts (dept_id INT PRIMARY KEY, dept_name VARCHAR)").unwrap();
    conn.execute("CREATE TABLE emps (emp_id INT PRIMARY KEY, dept_id INT, emp_name VARCHAR)").unwrap();

    conn.execute("INSERT INTO depts VALUES (10, 'Engineering'), (20, 'HR')").unwrap();
    conn.execute("INSERT INTO emps VALUES (1, 10, 'Alice'), (2, 10, 'Bob'), (3, 99, 'Ghost')").unwrap();

    let rows = conn.query("SELECT emps.emp_name, depts.dept_name FROM emps JOIN depts USING (dept_id) ORDER BY emps.emp_id").unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].get_as::<String>(0).unwrap(), "Alice");
    assert_eq!(rows[0].get_as::<String>(1).unwrap(), "Engineering");
    assert_eq!(rows[1].get_as::<String>(0).unwrap(), "Bob");
    assert_eq!(rows[1].get_as::<String>(1).unwrap(), "Engineering");
}

#[test]
fn test_natural_join() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute("CREATE TABLE categories (cat_id INT PRIMARY KEY, title VARCHAR)").unwrap();
    conn.execute("CREATE TABLE products (prod_id INT PRIMARY KEY, cat_id INT, p_name VARCHAR)").unwrap();

    conn.execute("INSERT INTO categories VALUES (1, 'Electronics'), (2, 'Books')").unwrap();
    conn.execute("INSERT INTO products VALUES (101, 1, 'Laptop'), (102, 2, 'Novel')").unwrap();

    let rows = conn.query("SELECT products.p_name, categories.title FROM products NATURAL JOIN categories ORDER BY products.prod_id").unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].get_as::<String>(0).unwrap(), "Laptop");
    assert_eq!(rows[0].get_as::<String>(1).unwrap(), "Electronics");
    assert_eq!(rows[1].get_as::<String>(0).unwrap(), "Novel");
    assert_eq!(rows[1].get_as::<String>(1).unwrap(), "Books");
}
