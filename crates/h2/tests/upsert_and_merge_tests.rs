use h2::Connection;

#[test]
fn test_upsert_on_conflict_do_nothing() {
    let conn = Connection::open_in_memory().unwrap();

    conn.execute(
        "CREATE TABLE players (
            id INT PRIMARY KEY,
            name VARCHAR(50),
            score INT
        )"
    ).unwrap();

    conn.execute("INSERT INTO players VALUES (1, 'Alice', 100)").unwrap();

    // ON CONFLICT DO NOTHING: 重複してもエラーにならず無視
    conn.execute("INSERT INTO players VALUES (1, 'Alice 2', 999) ON CONFLICT (id) DO NOTHING").unwrap();

    let rows = conn.query("SELECT score FROM players WHERE id = 1").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get_as::<i32>(0).unwrap(), 100); // 変更されていないこと
}

#[test]
fn test_upsert_on_conflict_do_update_with_excluded() {
    let conn = Connection::open_in_memory().unwrap();

    conn.execute(
        "CREATE TABLE counters (
            key VARCHAR(50) PRIMARY KEY,
            val INT
        )"
    ).unwrap();

    conn.execute("INSERT INTO counters VALUES ('page_views', 1)").unwrap();

    // ON CONFLICT DO UPDATE SET val = counters.val + EXCLUDED.val
    conn.execute(
        "INSERT INTO counters VALUES ('page_views', 5)
         ON CONFLICT (key) DO UPDATE SET val = counters.val + EXCLUDED.val"
    ).unwrap();

    let rows = conn.query("SELECT val FROM counters WHERE key = 'page_views'").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get_as::<i32>(0).unwrap(), 6);

    // 新規キーの挿入
    conn.execute(
        "INSERT INTO counters VALUES ('downloads', 10)
         ON CONFLICT (key) DO UPDATE SET val = counters.val + EXCLUDED.val"
    ).unwrap();

    let rows = conn.query("SELECT val FROM counters WHERE key = 'downloads'").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get_as::<i32>(0).unwrap(), 10);
}

#[test]
fn test_merge_into_statement() {
    let conn = Connection::open_in_memory().unwrap();

    conn.execute("CREATE TABLE target_inventory (item_id INT PRIMARY KEY, stock INT)").unwrap();
    conn.execute("CREATE TABLE incoming_shipments (item_id INT PRIMARY KEY, quantity INT)").unwrap();

    conn.execute("INSERT INTO target_inventory VALUES (101, 10)").unwrap();
    conn.execute("INSERT INTO target_inventory VALUES (102, 20)").unwrap();

    // 102 は既存（更新対象）、103 は新規（挿入対象）
    conn.execute("INSERT INTO incoming_shipments VALUES (102, 5)").unwrap();
    conn.execute("INSERT INTO incoming_shipments VALUES (103, 50)").unwrap();

    conn.execute(
        "MERGE INTO target_inventory t
         USING incoming_shipments s
         ON (t.item_id = s.item_id)
         WHEN MATCHED THEN
             UPDATE SET stock = t.stock + s.quantity
         WHEN NOT MATCHED THEN
             INSERT (item_id, stock) VALUES (s.item_id, s.quantity)"
    ).unwrap();

    let rows = conn.query("SELECT item_id, stock FROM target_inventory ORDER BY item_id").unwrap();
    assert_eq!(rows.len(), 3);

    // 101: 変更なし (10)
    assert_eq!(rows[0].get_as::<i32>(0).unwrap(), 101);
    assert_eq!(rows[0].get_as::<i32>(1).unwrap(), 10);

    // 102: マッチして加算 (20 + 5 = 25)
    assert_eq!(rows[1].get_as::<i32>(0).unwrap(), 102);
    assert_eq!(rows[1].get_as::<i32>(1).unwrap(), 25);

    // 103: 新規挿入 (50)
    assert_eq!(rows[2].get_as::<i32>(0).unwrap(), 103);
    assert_eq!(rows[2].get_as::<i32>(1).unwrap(), 50);
}
