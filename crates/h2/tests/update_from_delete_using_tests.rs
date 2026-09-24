use h2::Connection;

#[test]
fn test_update_from_join() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute("CREATE TABLE employees (id INT PRIMARY KEY, name VARCHAR, salary INT)").unwrap();
    conn.execute("CREATE TABLE bonuses (emp_id INT PRIMARY KEY, bonus_amount INT)").unwrap();

    conn.execute("INSERT INTO employees VALUES (1, 'Alice', 5000), (2, 'Bob', 4000), (3, 'Charlie', 4500)").unwrap();
    conn.execute("INSERT INTO bonuses VALUES (1, 1000), (2, 500)").unwrap();

    // UPDATE ... FROM
    let affected = conn.execute(
        "UPDATE employees
         SET salary = employees.salary + bonuses.bonus_amount
         FROM bonuses
         WHERE employees.id = bonuses.emp_id"
    ).unwrap();
    assert_eq!(affected, 2);

    let rows = conn.query("SELECT id, salary FROM employees ORDER BY id").unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].get_as::<i32>(1).unwrap(), 6000); // 5000 + 1000
    assert_eq!(rows[1].get_as::<i32>(1).unwrap(), 4500); // 4000 + 500
    assert_eq!(rows[2].get_as::<i32>(1).unwrap(), 4500); // unaffected
}

#[test]
fn test_update_from_with_returning() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute("CREATE TABLE items (id INT PRIMARY KEY, price INT)").unwrap();
    conn.execute("CREATE TABLE discounts (item_id INT PRIMARY KEY, rate INT)").unwrap();

    conn.execute("INSERT INTO items VALUES (1, 100), (2, 200)").unwrap();
    conn.execute("INSERT INTO discounts VALUES (1, 10), (2, 20)").unwrap();

    let rows = conn.query(
        "UPDATE items
         SET price = items.price - discounts.rate
         FROM discounts
         WHERE items.id = discounts.item_id
         RETURNING items.id, items.price"
    ).unwrap();

    assert_eq!(rows.len(), 2);
    let mut results: Vec<(i32, i32)> = rows.iter().map(|r| (r.get_as::<i32>(0).unwrap(), r.get_as::<i32>(1).unwrap())).collect();
    results.sort_by_key(|k| k.0);
    assert_eq!(results, vec![(1, 90), (2, 180)]);
}

#[test]
fn test_delete_using() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute("CREATE TABLE sessions (id INT PRIMARY KEY, user_id INT)").unwrap();
    conn.execute("CREATE TABLE banned_users (id INT PRIMARY KEY)").unwrap();

    conn.execute("INSERT INTO sessions VALUES (10, 1), (20, 2), (30, 3)").unwrap();
    conn.execute("INSERT INTO banned_users VALUES (1), (3)").unwrap();

    // DELETE ... USING
    let affected = conn.execute(
        "DELETE FROM sessions
         USING banned_users
         WHERE sessions.user_id = banned_users.id"
    ).unwrap();
    assert_eq!(affected, 2);

    let rows = conn.query("SELECT id, user_id FROM sessions").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get_as::<i32>(0).unwrap(), 20);
    assert_eq!(rows[0].get_as::<i32>(1).unwrap(), 2);
}

#[test]
fn test_delete_using_with_returning() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute("CREATE TABLE queue (id INT PRIMARY KEY, status VARCHAR)").unwrap();
    conn.execute("CREATE TABLE cancel_requests (queue_id INT PRIMARY KEY)").unwrap();

    conn.execute("INSERT INTO queue VALUES (1, 'pending'), (2, 'processing'), (3, 'pending')").unwrap();
    conn.execute("INSERT INTO cancel_requests VALUES (1), (2)").unwrap();

    let rows = conn.query(
        "DELETE FROM queue
         USING cancel_requests
         WHERE queue.id = cancel_requests.queue_id
         RETURNING queue.id"
    ).unwrap();

    assert_eq!(rows.len(), 2);
    let mut ids: Vec<i32> = rows.iter().map(|r| r.get_as::<i32>(0).unwrap()).collect();
    ids.sort();
    assert_eq!(ids, vec![1, 2]);

    let rows = conn.query("SELECT id FROM queue").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get_as::<i32>(0).unwrap(), 3);
}
