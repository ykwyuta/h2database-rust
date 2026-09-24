use std::time::Duration;
use h2::{Connection, H2Error};


#[test]
fn test_query_timeout_on_lock_wait() {
    let conn = Connection::open_in_memory().unwrap();
    // データベース全体のロックタイムアウトは長め（5000ms）にしておく
    conn.set_lock_timeout_ms(5000);

    conn.execute("CREATE TABLE products (id INTEGER PRIMARY KEY, name VARCHAR, stock INTEGER)").unwrap();
    conn.execute("INSERT INTO products VALUES (1, 'Widget', 100)").unwrap();

    let conn1 = conn.new_session();
    let conn2 = conn.new_session();

    // 1. Tx1 が id=1 を更新してロックを保持（コミットしない）
    conn1.execute("BEGIN").unwrap();
    conn1.execute("UPDATE products SET stock = 150 WHERE id = 1").unwrap();

    // 2. Tx2 が id=1 を更新しようとするが、クエリタイムアウトを 60ms に指定
    let res = conn2.execute_timeout(
        "UPDATE products SET stock = 200 WHERE id = 1",
        Duration::from_millis(60),
    );

    // 3. 全体のロックタイムアウト（5000ms）を待たずに、60ms でクエリタイムアウトすること！
    assert!(res.is_err(), "Query should timeout");
    match res.unwrap_err() {
        H2Error::QueryTimeout(msg) => {
            assert!(msg.contains("timed out"), "Message should mention timed out: {}", msg);
        }
        other => panic!("Expected H2Error::QueryTimeout, got: {:?}", other),
    }

    // 4. Tx1 は影響を受けず正常にコミットできる
    assert!(conn1.execute("COMMIT").is_ok());

    // 5. コミット後は Tx1 の変更が反映されている
    let rows = conn.query("SELECT stock FROM products WHERE id = 1").unwrap();
    assert_eq!(rows[0].get_as::<i32>(0).unwrap(), 150);
}

#[test]
fn test_query_timeout_normal_completion() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute("CREATE TABLE test_items (id INTEGER PRIMARY KEY, val VARCHAR)").unwrap();
    conn.execute("INSERT INTO test_items VALUES (1, 'Hello'), (2, 'World')").unwrap();

    // 十分なタイムアウト（3秒）を指定してクエリ実行
    let rows = conn.query_timeout("SELECT id, val FROM test_items ORDER BY id", Duration::from_secs(3)).unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].get_as::<String>(1).unwrap(), "Hello");
    assert_eq!(rows[1].get_as::<String>(1).unwrap(), "World");
}

#[test]
fn test_connection_default_query_timeout() {
    let conn = Connection::open_in_memory().unwrap();
    conn.set_lock_timeout_ms(5000);

    conn.execute("CREATE TABLE counter (id INTEGER PRIMARY KEY, count INTEGER)").unwrap();
    conn.execute("INSERT INTO counter VALUES (1, 10)").unwrap();

    let conn1 = conn.new_session();
    let conn2 = conn.new_session();

    // Tx1 がロック保持
    conn1.execute("BEGIN").unwrap();
    conn1.execute("UPDATE counter SET count = 20 WHERE id = 1").unwrap();

    // conn2 のセッションデフォルトタイムアウトを 60ms に設定
    conn2.set_query_timeout_ms(60);
    assert_eq!(conn2.query_timeout_duration(), Some(Duration::from_millis(60)));

    // 通常の execute を呼んでも、自動的に 60ms でタイムアウトする！
    let res = conn2.execute("UPDATE counter SET count = 30 WHERE id = 1");
    assert!(res.is_err());
    assert!(matches!(res.unwrap_err(), H2Error::QueryTimeout(_)));

    // タイムアウトを解除
    conn2.set_query_timeout(None);
    assert_eq!(conn2.query_timeout_duration(), None);

    conn1.execute("COMMIT").unwrap();
}

#[test]
fn test_sql_set_statement_timeout() {
    let conn = Connection::open_in_memory().unwrap();
    conn.set_lock_timeout_ms(5000);

    conn.execute("CREATE TABLE metrics (id INTEGER PRIMARY KEY, val INTEGER)").unwrap();
    conn.execute("INSERT INTO metrics VALUES (1, 100)").unwrap();

    let conn1 = conn.new_session();
    let conn2 = conn.new_session();

    conn1.execute("BEGIN").unwrap();
    conn1.execute("UPDATE metrics SET val = 200 WHERE id = 1").unwrap();

    // SQL文によるタイムアウト設定
    conn2.execute("SET statement_timeout = 60").unwrap();
    assert_eq!(conn2.query_timeout_duration(), Some(Duration::from_millis(60)));

    let res = conn2.execute("UPDATE metrics SET val = 300 WHERE id = 1");
    assert!(res.is_err());
    assert!(matches!(res.unwrap_err(), H2Error::QueryTimeout(_)));

    // 解除
    conn2.execute("SET statement_timeout = 0").unwrap();
    assert_eq!(conn2.query_timeout_duration(), None);

    conn1.execute("ROLLBACK").unwrap();
}

#[test]
fn test_query_timeout_scan_interruption() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute("CREATE TABLE big_data (id INTEGER PRIMARY KEY, v VARCHAR)").unwrap();
    // 100行挿入
    let mut inserts = Vec::new();
    for i in 1..=100 {
        inserts.push(format!("({}, 'val_{}')", i, i));
    }
    conn.execute(&format!("INSERT INTO big_data VALUES {}", inserts.join(", "))).unwrap();

    // タイムアウト0（即座に期限切れ）を指定してクエリ実行
    let res = conn.query_timeout("SELECT * FROM big_data", Duration::from_nanos(1));
    assert!(res.is_err());
    assert!(matches!(res.unwrap_err(), H2Error::QueryTimeout(_)));
}

#[cfg(feature = "async")]
#[tokio::test]
async fn test_async_query_timeout() {
    use h2::AsyncConnection;

    let conn = AsyncConnection::open_in_memory().await.unwrap();
    conn.execute("CREATE TABLE async_items (id INTEGER PRIMARY KEY, txt VARCHAR)").await.unwrap();
    conn.execute("INSERT INTO async_items VALUES (1, 'Async')").await.unwrap();

    // 正常実行
    let rows = conn.query_timeout("SELECT txt FROM async_items", Duration::from_secs(2)).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get_as::<String>(0).unwrap(), "Async");

    // タイムアウト発生
    let res = conn.query_timeout("SELECT txt FROM async_items", Duration::from_nanos(1)).await;
    assert!(res.is_err());
    assert!(matches!(res.unwrap_err(), H2Error::QueryTimeout(_)));
}

