use std::thread;
use std::time::Duration;
use h2::Connection;


#[test]
fn test_sql_transaction_deadlock_detection_and_cancel() {
    let conn = Connection::open_in_memory().unwrap();
    conn.set_lock_timeout_ms(3000);

    // テーブルの初期化
    conn.execute("CREATE TABLE accounts (id INTEGER PRIMARY KEY, name VARCHAR, balance INTEGER)").unwrap();
    conn.execute("INSERT INTO accounts VALUES (1, 'Alice', 1000), (2, 'Bob', 2000)").unwrap();

    let conn1 = conn.new_session();
    let conn2 = conn.new_session();

    // 1. 各セッションで明示的トランザクションを開始
    conn1.execute("BEGIN").unwrap();
    conn2.execute("BEGIN").unwrap();

    // 2. Tx1 が id=1 をロック、Tx2 が id=2 をロック
    assert_eq!(conn1.execute("UPDATE accounts SET balance = 1100 WHERE id = 1").unwrap(), 1);
    assert_eq!(conn2.execute("UPDATE accounts SET balance = 2200 WHERE id = 2").unwrap(), 1);

    // 3. スレッドで Tx1 が id=2 を更新要求（Tx2がロック中のため待機に入る）
    let conn1_handle = conn1.clone();
    let thread1 = thread::spawn(move || {
        let res = conn1_handle.execute("UPDATE accounts SET balance = 1200 WHERE id = 2");
        if res.is_ok() {
            conn1_handle.execute("COMMIT").unwrap();
        }
        res
    });

    // Tx1 が確実にロック待機状態に入るよう少しスリープ
    thread::sleep(Duration::from_millis(60));

    // 4. メインスレッドで Tx2 が id=1 を更新要求（Tx1がロック中）
    // -> Wait-For Graph: Tx2 -> Tx1 -> Tx2 の循環依存（デッドロック）を即座に検出！
    // -> 要求元の Tx2 を Victim として自動ロールバック（キャンセル）し、LockConflict エラーを返却
    let res2 = conn2.execute("UPDATE accounts SET balance = 2100 WHERE id = 1");

    assert!(res2.is_err(), "Tx2 should fail due to deadlock detection");
    let err_msg = res2.unwrap_err().to_string();
    assert!(
        err_msg.contains("Deadlock detected"),
        "Error message should mention deadlock detection, got: {}",
        err_msg
    );

    // キャンセルされた Tx2 のクリーンアップ（ROLLBACK発行）
    assert!(conn2.execute("ROLLBACK").is_ok());

    // 5. Tx2 が自動ロールバックされたことで id=2 のロックが解放され、
    //    待機していた Tx1（スレッド1）のブロックが解除されて正常に完了・コミットされる
    let thread1_res = thread1.join().unwrap();
    assert!(thread1_res.is_ok(), "Tx1 should successfully finish after Tx2 is cancelled: {:?}", thread1_res);

    // 6. 最終状態の検証
    // id=1: 1100 (Tx1の変更が反映)
    // id=2: 1200 (Tx1の変更が反映。Tx2の2200はキャンセル・ロールバックされたため破棄)
    let rows = conn.query("SELECT id, balance FROM accounts ORDER BY id").unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].get_as::<i32>(1).unwrap(), 1100);
    assert_eq!(rows[1].get_as::<i32>(1).unwrap(), 1200);
}

#[test]
fn test_sql_deadlock_three_way_cycle() {
    let conn = Connection::open_in_memory().unwrap();
    conn.set_lock_timeout_ms(3000);

    conn.execute("CREATE TABLE resources (id INTEGER PRIMARY KEY, val VARCHAR)").unwrap();
    conn.execute("INSERT INTO resources VALUES (1, 'R1'), (2, 'R2'), (3, 'R3')").unwrap();

    let conn1 = conn.new_session();
    let conn2 = conn.new_session();
    let conn3 = conn.new_session();

    conn1.execute("BEGIN").unwrap();
    conn2.execute("BEGIN").unwrap();
    conn3.execute("BEGIN").unwrap();

    // Tx1 locks R1, Tx2 locks R2, Tx3 locks R3
    conn1.execute("UPDATE resources SET val = 'T1_R1' WHERE id = 1").unwrap();
    conn2.execute("UPDATE resources SET val = 'T2_R2' WHERE id = 2").unwrap();
    conn3.execute("UPDATE resources SET val = 'T3_R3' WHERE id = 3").unwrap();

    // Tx1 waits for R2 (held by Tx2)
    let c1 = conn1.clone();
    let t1 = thread::spawn(move || {
        let r = c1.execute("UPDATE resources SET val = 'T1_R2' WHERE id = 2");
        if r.is_ok() {
            c1.execute("COMMIT").unwrap();
        }
        r
    });
    thread::sleep(Duration::from_millis(50));

    // Tx2 waits for R3 (held by Tx3)
    let c2 = conn2.clone();
    let t2 = thread::spawn(move || {
        let r = c2.execute("UPDATE resources SET val = 'T2_R3' WHERE id = 3");
        if r.is_ok() {
            c2.execute("COMMIT").unwrap();
        }
        r
    });
    thread::sleep(Duration::from_millis(50));

    // Tx3 requests R1 (held by Tx1)
    // -> Cycle: Tx3 -> Tx1 -> Tx2 -> Tx3 !
    // -> Tx3 is detected as causing deadlock and cancelled!
    let r3 = conn3.execute("UPDATE resources SET val = 'T3_R1' WHERE id = 1");
    assert!(r3.is_err(), "Tx3 must be cancelled due to 3-way deadlock");
    assert!(r3.unwrap_err().to_string().contains("Deadlock detected"));

    // Tx3 cancelled, releases R3 lock -> Tx2 unblocks and commits
    let t2_res = t2.join().unwrap();
    assert!(t2_res.is_ok(), "Tx2 unblocks and commits: {:?}", t2_res);

    // Tx2 committed, releases R2 lock -> Tx1 unblocks and commits
    let t1_res = t1.join().unwrap();
    assert!(t1_res.is_ok(), "Tx1 unblocks and commits: {:?}", t1_res);

    // Verify consistency
    let rows = conn.query("SELECT id, val FROM resources ORDER BY id").unwrap();
    assert_eq!(rows[0].get_as::<String>(1).unwrap(), "T1_R1");
    assert_eq!(rows[1].get_as::<String>(1).unwrap(), "T1_R2");
    assert_eq!(rows[2].get_as::<String>(1).unwrap(), "T2_R3");
}
