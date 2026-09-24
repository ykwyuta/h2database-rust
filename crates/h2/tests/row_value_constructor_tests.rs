use h2::Connection;

#[test]
fn test_row_value_equality_and_order() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute("CREATE TABLE points (x INT, y INT)").unwrap();
    conn.execute("INSERT INTO points VALUES (1, 2), (1, 5), (2, 1), (3, 3)").unwrap();

    // 1. (x, y) = (1, 2)
    let rows = conn.query("SELECT x, y FROM points WHERE (x, y) = (1, 2)").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get_as::<i32>(0).unwrap(), 1);
    assert_eq!(rows[0].get_as::<i32>(1).unwrap(), 2);

    // 2. (x, y) > (1, 2) => (1, 5), (2, 1), (3, 3)
    let rows_gt = conn.query("SELECT x, y FROM points WHERE (x, y) > (1, 2) ORDER BY x, y").unwrap();
    assert_eq!(rows_gt.len(), 3);
    assert_eq!(rows_gt[0].get_as::<i32>(0).unwrap(), 1);
    assert_eq!(rows_gt[0].get_as::<i32>(1).unwrap(), 5);
    assert_eq!(rows_gt[1].get_as::<i32>(0).unwrap(), 2);
    assert_eq!(rows_gt[1].get_as::<i32>(1).unwrap(), 1);
    assert_eq!(rows_gt[2].get_as::<i32>(0).unwrap(), 3);
    assert_eq!(rows_gt[2].get_as::<i32>(1).unwrap(), 3);
}

#[test]
fn test_row_value_in_list() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute("CREATE TABLE users (tenant_id INT, user_id INT, name VARCHAR)").unwrap();
    conn.execute("INSERT INTO users VALUES (1, 10, 'Alice'), (1, 20, 'Bob'), (2, 10, 'Charlie')").unwrap();

    // (tenant_id, user_id) IN ((1, 10), (2, 10))
    let rows = conn.query(
        "SELECT name FROM users WHERE (tenant_id, user_id) IN ((1, 10), (2, 10)) ORDER BY name"
    ).unwrap();

    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].get_as::<String>(0).unwrap(), "Alice");
    assert_eq!(rows[1].get_as::<String>(0).unwrap(), "Charlie");

    // NOT IN
    let rows_not_in = conn.query(
        "SELECT name FROM users WHERE (tenant_id, user_id) NOT IN ((1, 10), (2, 10))"
    ).unwrap();
    assert_eq!(rows_not_in.len(), 1);
    assert_eq!(rows_not_in[0].get_as::<String>(0).unwrap(), "Bob");
}

#[test]
fn test_row_value_in_subquery() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute("CREATE TABLE coordinates (lat INT, lon INT, name VARCHAR)").unwrap();
    conn.execute("CREATE TABLE targets (t_lat INT, t_lon INT)").unwrap();

    conn.execute("INSERT INTO coordinates VALUES (10, 20, 'City A'), (30, 40, 'City B'), (50, 60, 'City C')").unwrap();
    conn.execute("INSERT INTO targets VALUES (10, 20), (50, 60)").unwrap();

    let rows = conn.query(
        "SELECT name FROM coordinates WHERE (lat, lon) IN (SELECT t_lat, t_lon FROM targets) ORDER BY name"
    ).unwrap();

    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].get_as::<String>(0).unwrap(), "City A");
    assert_eq!(rows[1].get_as::<String>(0).unwrap(), "City C");
}
