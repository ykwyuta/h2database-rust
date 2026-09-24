use h2::Connection;

#[test]
fn test_set_operations_intersect_and_except() {
    let conn = Connection::open_in_memory().expect("open memory db");

    conn.execute("CREATE TABLE t1 (id INT PRIMARY KEY, val INT);")
        .unwrap();
    conn.execute("CREATE TABLE t2 (id INT PRIMARY KEY, val INT);")
        .unwrap();

    conn.execute("INSERT INTO t1 VALUES (1, 10), (2, 20), (3, 30), (4, 40);")
        .unwrap();
    conn.execute("INSERT INTO t2 VALUES (1, 30), (2, 40), (3, 50), (4, 60);")
        .unwrap();

    // 1. INTERSECT (共通行 30, 40)
    let rows = conn
        .query("SELECT val FROM t1 INTERSECT SELECT val FROM t2 ORDER BY val ASC;")
        .unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].get_as::<i32>(0).unwrap(), 30);
    assert_eq!(rows[1].get_as::<i32>(0).unwrap(), 40);

    // 2. EXCEPT (t1 から t2 を除外 -> 10, 20)
    let rows = conn
        .query("SELECT val FROM t1 EXCEPT SELECT val FROM t2 ORDER BY val ASC;")
        .unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].get_as::<i32>(0).unwrap(), 10);
    assert_eq!(rows[1].get_as::<i32>(0).unwrap(), 20);

    // 3. EXCEPT の逆 (t2 から t1 を除外 -> 50, 60)
    let rows = conn
        .query("SELECT val FROM t2 EXCEPT SELECT val FROM t1 ORDER BY val ASC;")
        .unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].get_as::<i32>(0).unwrap(), 50);
    assert_eq!(rows[1].get_as::<i32>(0).unwrap(), 60);

    // 4. 重複行を含む ALL の挙動検証
    conn.execute("CREATE TABLE dup1 (val INT);").unwrap();
    conn.execute("CREATE TABLE dup2 (val INT);").unwrap();
    conn.execute("INSERT INTO dup1 VALUES (1), (1), (2), (2), (2), (3);").unwrap();
    conn.execute("INSERT INTO dup2 VALUES (1), (2), (2), (4);").unwrap();

    // INTERSECT ALL: 1 は 1個、2 は 2個
    let rows = conn
        .query("SELECT val FROM dup1 INTERSECT ALL SELECT val FROM dup2 ORDER BY val ASC;")
        .unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].get_as::<i32>(0).unwrap(), 1);
    assert_eq!(rows[1].get_as::<i32>(0).unwrap(), 2);
    assert_eq!(rows[2].get_as::<i32>(0).unwrap(), 2);

    // EXCEPT ALL: dup1 から dup2 を減算 -> 1が1個 (2-1), 2が1個 (3-2), 3が1個 (1-0)
    let rows = conn
        .query("SELECT val FROM dup1 EXCEPT ALL SELECT val FROM dup2 ORDER BY val ASC;")
        .unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].get_as::<i32>(0).unwrap(), 1);
    assert_eq!(rows[1].get_as::<i32>(0).unwrap(), 2);
    assert_eq!(rows[2].get_as::<i32>(0).unwrap(), 3);
}

#[test]
fn test_timestamptz_data_type() {
    let conn = Connection::open_in_memory().expect("open memory db");

    conn.execute(
        "CREATE TABLE audit_logs (id INT PRIMARY KEY, action VARCHAR, event_time TIMESTAMPTZ);",
    )
    .unwrap();

    // JST (+09:00) の 12:00 は UTC 03:00 と一致する
    conn.execute(
        "INSERT INTO audit_logs VALUES (1, 'login_jst', '2026-09-24T12:00:00+09:00');",
    )
    .unwrap();

    // UTC (Z) の 03:00
    conn.execute(
        "INSERT INTO audit_logs VALUES (2, 'login_utc', '2026-09-24 03:00:00Z');",
    )
    .unwrap();

    // EST (-05:00) の前日 22:00 は UTC 03:00 と一致する
    conn.execute(
        "INSERT INTO audit_logs VALUES (3, 'login_est', '2026-09-23 22:00:00-05:00');",
    )
    .unwrap();

    // 異なる時刻
    conn.execute(
        "INSERT INTO audit_logs VALUES (4, 'logout', '2026-09-24 15:00:00Z');",
    )
    .unwrap();

    // 1, 2, 3 の行はいずれも UTC 2026-09-24 03:00:00 と等価
    let rows = conn
        .query(
            "SELECT id, action FROM audit_logs WHERE event_time = '2026-09-24T03:00:00Z' ORDER BY id ASC;",
        )
        .unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].get_as::<i32>(0).unwrap(), 1);
    assert_eq!(rows[1].get_as::<i32>(0).unwrap(), 2);
    assert_eq!(rows[2].get_as::<i32>(0).unwrap(), 3);

    // 範囲比較
    let rows = conn
        .query(
            "SELECT id FROM audit_logs WHERE event_time > '2026-09-24 04:00:00Z';",
        )
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get_as::<i32>(0).unwrap(), 4);
}

#[test]
fn test_window_functions_row_number_rank_dense_rank() {
    let conn = Connection::open_in_memory().expect("open memory db");

    conn.execute(
        "CREATE TABLE leaderboard (id INT PRIMARY KEY, dept VARCHAR, score INT);",
    )
    .unwrap();

    conn.execute(
        "INSERT INTO leaderboard VALUES
            (1, 'Sales', 100),
            (2, 'Sales', 90),
            (3, 'Sales', 90),
            (4, 'Sales', 70),
            (5, 'Eng', 95),
            (6, 'Eng', 85);",
    )
    .unwrap();

    // PARTITION BY dept ORDER BY score DESC
    let rows = conn
        .query(
            "SELECT id, dept, score,
                    ROW_NUMBER() OVER (PARTITION BY dept ORDER BY score DESC) as rn,
                    RANK() OVER (PARTITION BY dept ORDER BY score DESC) as rk,
                    DENSE_RANK() OVER (PARTITION BY dept ORDER BY score DESC) as drk
             FROM leaderboard
             ORDER BY dept ASC, rn ASC;",
        )
        .unwrap();

    assert_eq!(rows.len(), 6);

    // Eng グループ
    // 1行目: id=5, score=95 -> rn=1, rk=1, drk=1
    assert_eq!(rows[0].get_as::<String>(1).unwrap(), "Eng");
    assert_eq!(rows[0].get_as::<i32>(0).unwrap(), 5);
    assert_eq!(rows[0].get_as::<i64>(3).unwrap(), 1);
    assert_eq!(rows[0].get_as::<i64>(4).unwrap(), 1);
    assert_eq!(rows[0].get_as::<i64>(5).unwrap(), 1);

    // 2行目: id=6, score=85 -> rn=2, rk=2, drk=2
    assert_eq!(rows[1].get_as::<i32>(0).unwrap(), 6);
    assert_eq!(rows[1].get_as::<i64>(3).unwrap(), 2);
    assert_eq!(rows[1].get_as::<i64>(4).unwrap(), 2);
    assert_eq!(rows[1].get_as::<i64>(5).unwrap(), 2);

    // Sales グループ
    // 3行目: id=1, score=100 -> rn=1, rk=1, drk=1
    assert_eq!(rows[2].get_as::<String>(1).unwrap(), "Sales");
    assert_eq!(rows[2].get_as::<i32>(0).unwrap(), 1);
    assert_eq!(rows[2].get_as::<i64>(3).unwrap(), 1);
    assert_eq!(rows[2].get_as::<i64>(4).unwrap(), 1);
    assert_eq!(rows[2].get_as::<i64>(5).unwrap(), 1);

    // 4行目: id=2, score=90 -> rn=2, rk=2, drk=2 (同点)
    assert_eq!(rows[3].get_as::<i64>(3).unwrap(), 2);
    assert_eq!(rows[3].get_as::<i64>(4).unwrap(), 2);
    assert_eq!(rows[3].get_as::<i64>(5).unwrap(), 2);

    // 5行目: id=3, score=90 -> rn=3, rk=2, drk=2 (同点)
    assert_eq!(rows[4].get_as::<i64>(3).unwrap(), 3);
    assert_eq!(rows[4].get_as::<i64>(4).unwrap(), 2);
    assert_eq!(rows[4].get_as::<i64>(5).unwrap(), 2);

    // 6行目: id=4, score=70 -> rn=4, rk=4 (タイにより3をスキップ), drk=3
    assert_eq!(rows[5].get_as::<i32>(0).unwrap(), 4);
    assert_eq!(rows[5].get_as::<i64>(3).unwrap(), 4);
    assert_eq!(rows[5].get_as::<i64>(4).unwrap(), 4);
    assert_eq!(rows[5].get_as::<i64>(5).unwrap(), 3);

    // 全体単一ソートの ROW_NUMBER
    let overall_rows = conn
        .query(
            "SELECT id, ROW_NUMBER() OVER (ORDER BY score DESC) as rn FROM leaderboard ORDER BY rn ASC;",
        )
        .unwrap();
    assert_eq!(overall_rows.len(), 6);
    assert_eq!(overall_rows[0].get_as::<i32>(0).unwrap(), 1); // score 100
    assert_eq!(overall_rows[0].get_as::<i64>(1).unwrap(), 1);
}

#[test]
fn test_create_and_drop_view() {
    let conn = Connection::open_in_memory().expect("open memory db");

    conn.execute(
        "CREATE TABLE users (id INT PRIMARY KEY, name VARCHAR, role VARCHAR, is_active BOOLEAN);",
    )
    .unwrap();
    conn.execute(
        "CREATE TABLE orders (id INT PRIMARY KEY, user_id INT, amount INT);",
    )
    .unwrap();

    conn.execute(
        "INSERT INTO users VALUES
            (1, 'Alice', 'admin', true),
            (2, 'Bob', 'member', true),
            (3, 'Charlie', 'member', false);",
    )
    .unwrap();

    conn.execute(
        "INSERT INTO orders VALUES
            (101, 1, 500),
            (102, 1, 1500),
            (103, 2, 800),
            (104, 3, 2000);",
    )
    .unwrap();

    // 1. 基本ビューの作成
    conn.execute(
        "CREATE VIEW active_members AS SELECT id, name, role FROM users WHERE is_active = true;",
    )
    .unwrap();

    // ビューに対する単純クエリ
    let rows = conn
        .query("SELECT id, name FROM active_members ORDER BY id ASC;")
        .unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].get_as::<String>(1).unwrap(), "Alice");
    assert_eq!(rows[1].get_as::<String>(1).unwrap(), "Bob");

    // 2. ビューとテーブルの JOIN
    let rows = conn
        .query(
            "SELECT m.name, o.amount FROM active_members m JOIN orders o ON m.id = o.user_id ORDER BY o.amount ASC;",
        )
        .unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].get_as::<String>(0).unwrap(), "Alice");
    assert_eq!(rows[0].get_as::<i32>(1).unwrap(), 500);
    assert_eq!(rows[1].get_as::<String>(0).unwrap(), "Bob");
    assert_eq!(rows[1].get_as::<i32>(1).unwrap(), 800);
    assert_eq!(rows[2].get_as::<String>(0).unwrap(), "Alice");
    assert_eq!(rows[2].get_as::<i32>(1).unwrap(), 1500);

    // 3. CREATE OR REPLACE VIEW
    conn.execute(
        "CREATE OR REPLACE VIEW active_members AS SELECT id, name FROM users WHERE is_active = true AND role = 'admin';",
    )
    .unwrap();

    let rows = conn
        .query("SELECT name FROM active_members;")
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get_as::<String>(0).unwrap(), "Alice");

    // 4. DROP VIEW
    conn.execute("DROP VIEW active_members;").unwrap();

    // 削除済みビューへのクエリはエラー
    let res = conn.query("SELECT * FROM active_members;");
    assert!(res.is_err());

    // DROP VIEW IF EXISTS
    assert!(conn.execute("DROP VIEW IF EXISTS active_members;").is_ok());
}
