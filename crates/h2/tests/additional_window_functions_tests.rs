use h2::Connection;
use h2_types::Value;

#[test]
fn test_window_lead_and_lag() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute("CREATE TABLE sales (day INT PRIMARY KEY, revenue INT)").unwrap();
    conn.execute("INSERT INTO sales VALUES (1, 100), (2, 150), (3, 200), (4, 250)").unwrap();

    let rows = conn.query(
        "SELECT day, revenue,
                LAG(revenue) OVER (ORDER BY day) AS prev_rev,
                LEAD(revenue) OVER (ORDER BY day) AS next_rev
         FROM sales
         ORDER BY day"
    ).unwrap();

    assert_eq!(rows.len(), 4);

    // Day 1: LAG is NULL, LEAD is 150
    assert_eq!(rows[0].get(2), Some(&Value::Null));
    assert_eq!(rows[0].get_as::<i32>(3).unwrap(), 150);

    // Day 2: LAG is 100, LEAD is 200
    assert_eq!(rows[1].get_as::<i32>(2).unwrap(), 100);
    assert_eq!(rows[1].get_as::<i32>(3).unwrap(), 200);

    // Day 4: LAG is 200, LEAD is NULL
    assert_eq!(rows[3].get_as::<i32>(2).unwrap(), 200);
    assert_eq!(rows[3].get(3), Some(&Value::Null));

    // With offset and default value
    let rows_custom = conn.query(
        "SELECT day,
                LAG(revenue, 2, 0) OVER (ORDER BY day) AS lag2,
                LEAD(revenue, 2, -1) OVER (ORDER BY day) AS lead2
         FROM sales
         ORDER BY day"
    ).unwrap();
    assert_eq!(rows_custom[0].get_as::<i32>(1).unwrap(), 0); // lag2 fallback
    assert_eq!(rows_custom[0].get_as::<i32>(2).unwrap(), 200); // day 1 + 2 = day 3 (200)
    assert_eq!(rows_custom[3].get_as::<i32>(2).unwrap(), -1); // lead2 fallback
}

#[test]
fn test_window_first_value_last_value() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute("CREATE TABLE scores (dept VARCHAR, name VARCHAR, score INT)").unwrap();
    conn.execute("INSERT INTO scores VALUES ('A', 'Alice', 90), ('A', 'Bob', 80), ('B', 'Charlie', 70), ('B', 'Dave', 95)").unwrap();

    let rows = conn.query(
        "SELECT dept, name, score,
                FIRST_VALUE(score) OVER (PARTITION BY dept ORDER BY score DESC) AS highest,
                LAST_VALUE(score) OVER (PARTITION BY dept ORDER BY score DESC) AS lowest
         FROM scores
         ORDER BY dept, score DESC"
    ).unwrap();

    assert_eq!(rows.len(), 4);
    // Dept A (Alice 90, Bob 80)
    assert_eq!(rows[0].get_as::<i32>(3).unwrap(), 90);
    assert_eq!(rows[0].get_as::<i32>(4).unwrap(), 80);
    assert_eq!(rows[1].get_as::<i32>(3).unwrap(), 90);
    assert_eq!(rows[1].get_as::<i32>(4).unwrap(), 80);

    // Dept B (Dave 95, Charlie 70)
    assert_eq!(rows[2].get_as::<i32>(3).unwrap(), 95);
    assert_eq!(rows[2].get_as::<i32>(4).unwrap(), 70);
    assert_eq!(rows[3].get_as::<i32>(3).unwrap(), 95);
    assert_eq!(rows[3].get_as::<i32>(4).unwrap(), 70);
}

#[test]
fn test_window_ntile() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute("CREATE TABLE nums (id INT PRIMARY KEY)").unwrap();
    for i in 1..=7 {
        conn.execute(&format!("INSERT INTO nums VALUES ({})", i)).unwrap();
    }

    // 7 rows into 3 buckets: sizes should be 3, 2, 2
    let rows = conn.query(
        "SELECT id, NTILE(3) OVER (ORDER BY id) AS tile FROM nums ORDER BY id"
    ).unwrap();

    assert_eq!(rows.len(), 7);
    let tiles: Vec<i64> = rows.iter().map(|r| r.get_as::<i64>(1).unwrap()).collect();
    assert_eq!(tiles, vec![1, 1, 1, 2, 2, 3, 3]);
}
