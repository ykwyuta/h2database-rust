use std::thread::sleep;
use std::time::Duration;
use h2::{Connection, Value};

#[test]
fn test_cache_table_ddl_and_pseudo_columns() {
    let conn = Connection::open_in_memory().unwrap();

    // 1. 永続化バッキングテーブル作成
    conn.execute(
        "CREATE TABLE persistent_sessions (
            session_id VARCHAR(64) PRIMARY KEY,
            user_id BIGINT,
            payload TEXT
        );",
    )
    .unwrap();

    // 2. キャッシュテーブル作成 (WITH 句付き)
    conn.execute(
        "CREATE CACHE TABLE cache_sessions (
            session_id VARCHAR(64) PRIMARY KEY,
            user_id BIGINT,
            payload TEXT
        ) WITH (
            TTL = '10s',
            WRITE_BACK_TABLE = 'persistent_sessions',
            WRITE_BACK_INTERVAL = '1s',
            WRITE_BACK_MODE = 'BOTH',
            UNLOGGED = TRUE
        );",
    )
    .unwrap();

    // 3. データ挿入
    conn.execute(
        "INSERT INTO cache_sessions (session_id, user_id, payload)
         VALUES ('sess_1', 101, 'login_data_1'), ('sess_2', 102, 'login_data_2');",
    )
    .unwrap();

    // 4. クエリ確認
    let rows = conn.query("SELECT session_id, user_id, payload, _dirty FROM cache_sessions ORDER BY session_id;").unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].get(0), Some(&Value::String("sess_1".to_string())));
    assert_eq!(rows[0].get(1), Some(&Value::BigInt(101)));
    assert_eq!(rows[0].get(2), Some(&Value::String("login_data_1".to_string())));
    assert_eq!(rows[0].get(3), Some(&Value::Boolean(true))); // _dirty flag is true
}

#[test]
fn test_cache_table_ttl_active_filtering() {
    let conn = Connection::open_in_memory().unwrap();

    // 短い TTL (500ms) のキャッシュテーブル作成
    conn.execute(
        "CREATE CACHE TABLE temp_cache (
            key VARCHAR(32) PRIMARY KEY,
            val TEXT
        ) WITH (
            TTL = '400ms'
        );",
    )
    .unwrap();

    conn.execute("INSERT INTO temp_cache (key, val) VALUES ('k1', 'val1');").unwrap();

    // 即座にクエリ -> 取得可能
    let rows = conn.query("SELECT key, val FROM temp_cache;").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get(0), Some(&Value::String("k1".to_string())));

    // 600ms 待機 -> TTL 400ms 超過で自然失効
    sleep(Duration::from_millis(600));

    // 失効後はクエリで透過的に不可視化
    let expired_rows = conn.query("SELECT key, val FROM temp_cache;").unwrap();
    assert_eq!(expired_rows.len(), 0);

    let count_rows = conn.query("SELECT COUNT(*) FROM temp_cache;").unwrap();
    assert_eq!(count_rows[0].get(0), Some(&Value::BigInt(0)));
}

#[test]
fn test_cache_table_touch_expiration_extension() {
    let conn = Connection::open_in_memory().unwrap();

    conn.execute(
        "CREATE CACHE TABLE touch_sessions (
            id INT PRIMARY KEY,
            val VARCHAR(32)
        ) WITH (
            TTL = '500ms'
        );",
    )
    .unwrap();

    conn.execute("INSERT INTO touch_sessions (id, val) VALUES (1, 'initial');").unwrap();

    // 250ms 経過時点で TOUCH 文を発行し、さらに 1000ms 延長
    sleep(Duration::from_millis(250));
    conn.execute("TOUCH touch_sessions WHERE id = 1 EXTEND '1s';").unwrap();

    // さらに 400ms 待機（合計 650ms 経過: 元の 500ms を超過しているが延長されたため有効）
    sleep(Duration::from_millis(400));
    let rows = conn.query("SELECT id, val FROM touch_sessions WHERE id = 1;").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get(1), Some(&Value::String("initial".to_string())));

    // さらに 1000ms 待機（合計 1650ms: 延長分も超過して失効）
    sleep(Duration::from_millis(1000));
    let expired_rows = conn.query("SELECT id, val FROM touch_sessions WHERE id = 1;").unwrap();
    assert_eq!(expired_rows.len(), 0);
}

#[test]
fn test_cache_table_write_behind_flush_and_delta() {
    let conn = Connection::open_in_memory().unwrap();

    // 永続化テーブル
    conn.execute(
        "CREATE TABLE disk_store (
            item_id INT PRIMARY KEY,
            qty INT,
            tag VARCHAR(32)
        );",
    )
    .unwrap();

    // キャッシュテーブル
    conn.execute(
        "CREATE CACHE TABLE mem_store (
            item_id INT PRIMARY KEY,
            qty INT,
            tag VARCHAR(32)
        ) WITH (
            TTL = '10s',
            WRITE_BACK_TABLE = 'disk_store',
            WRITE_BACK_MODE = 'UPSERT'
        );",
    )
    .unwrap();

    // 永続化テーブルは初期空
    let init_disk = conn.query("SELECT * FROM disk_store;").unwrap();
    assert_eq!(init_disk.len(), 0);

    // キャッシュに挿入
    conn.execute("INSERT INTO mem_store (item_id, qty, tag) VALUES (1, 10, 'A'), (2, 20, 'B');").unwrap();

    // 手動 FLUSH WRITE_BACK
    conn.execute("FLUSH WRITE_BACK mem_store;").unwrap();

    // 永続化テーブルに反映されたことを検証
    let disk_rows = conn.query("SELECT item_id, qty, tag FROM disk_store ORDER BY item_id;").unwrap();
    assert_eq!(disk_rows.len(), 2);
    assert_eq!(disk_rows[0].get(0), Some(&Value::Integer(1)));
    assert_eq!(disk_rows[0].get(1), Some(&Value::Integer(10)));
    assert_eq!(disk_rows[1].get(0), Some(&Value::Integer(2)));
    assert_eq!(disk_rows[1].get(1), Some(&Value::Integer(20)));

    // キャッシュ内の _dirty フラグが false にリセットされていることを確認
    let mem_rows = conn.query("SELECT item_id, _dirty FROM mem_store ORDER BY item_id;").unwrap();
    assert_eq!(mem_rows[0].get(1), Some(&Value::Boolean(false)));
    assert_eq!(mem_rows[1].get(1), Some(&Value::Boolean(false)));

    // item_id = 1 のみ更新 -> item_id = 1 のみ _dirty = true になる
    conn.execute("UPDATE mem_store SET qty = 99 WHERE item_id = 1;").unwrap();
    let mem_dirty = conn.query("SELECT item_id, _dirty FROM mem_store ORDER BY item_id;").unwrap();
    assert_eq!(mem_dirty[0].get(1), Some(&Value::Boolean(true)));
    assert_eq!(mem_dirty[1].get(1), Some(&Value::Boolean(false)));

    // 再度 FLUSH
    conn.execute("FLUSH WRITE_BACK mem_store;").unwrap();

    // 永続化テーブルの item_id = 1 が更新されたことを確認
    let disk_updated = conn.query("SELECT item_id, qty FROM disk_store WHERE item_id = 1;").unwrap();
    assert_eq!(disk_updated[0].get(1), Some(&Value::Integer(99)));
}

#[test]
fn test_cache_table_expiration_and_delete_propagation() {
    let conn = Connection::open_in_memory().unwrap();

    conn.execute(
        "CREATE TABLE backing_tokens (
            token VARCHAR(32) PRIMARY KEY,
            username VARCHAR(32)
        );",
    )
    .unwrap();

    conn.execute(
        "CREATE CACHE TABLE cache_tokens (
            token VARCHAR(32) PRIMARY KEY,
            username VARCHAR(32)
        ) WITH (
            TTL = '500ms',
            WRITE_BACK_TABLE = 'backing_tokens',
            WRITE_BACK_MODE = 'BOTH'
        );",
    )
    .unwrap();

    conn.execute("INSERT INTO cache_tokens (token, username) VALUES ('t1', 'alice');").unwrap();
    conn.execute("FLUSH WRITE_BACK cache_tokens;").unwrap();

    // 永続化テーブルに t1 が存在する
    let disk_rows = conn.query("SELECT token, username FROM backing_tokens;").unwrap();
    assert_eq!(disk_rows.len(), 1);

    // TTL 経過で失効
    sleep(Duration::from_millis(600));

    // FLUSH 実行時、失効した行がバッキングテーブルからも削除される (WRITE_BACK_MODE = 'BOTH')
    conn.execute("FLUSH WRITE_BACK cache_tokens;").unwrap();

    let disk_after = conn.query("SELECT token, username FROM backing_tokens;").unwrap();
    assert_eq!(disk_after.len(), 0);
}

#[test]
fn test_background_cache_write_behind_cleaner() {
    let conn = Connection::open_in_memory().unwrap();

    conn.execute(
        "CREATE TABLE bg_disk (
            id INT PRIMARY KEY,
            data TEXT
        );",
    )
    .unwrap();

    conn.execute(
        "CREATE CACHE TABLE bg_cache (
            id INT PRIMARY KEY,
            data TEXT
        ) WITH (
            TTL = '10s',
            WRITE_BACK_TABLE = 'bg_disk',
            WRITE_BACK_INTERVAL = '100ms'
        );",
    )
    .unwrap();

    // バックグラウンドワーカー起動 (100ms 間隔)
    let _cleaner = conn.start_cache_write_behind(Duration::from_millis(100));

    conn.execute("INSERT INTO bg_cache (id, data) VALUES (10, 'async_write_behind');").unwrap();

    // 300ms 待機（明示的 FLUSH は実行しない）
    sleep(Duration::from_millis(300));

    // バックグラウンドスレッドによって永続化テーブルに自動反映されていることを検証
    let rows = conn.query("SELECT id, data FROM bg_disk WHERE id = 10;").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get(0), Some(&Value::Integer(10)));
    assert_eq!(rows[0].get(1), Some(&Value::String("async_write_behind".to_string())));
}
