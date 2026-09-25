use h2::Connection;
use h2_mvstore::buffer_pool::{BufferPoolManager, DiskManager, SlottedPage};
use h2_sql::memory::MemoryGrantCoordinator;
use std::sync::Arc;
use std::time::Duration;

#[test]
fn test_statistics_and_analyze() {
    let conn = Connection::open_in_memory().unwrap();

    conn.execute("CREATE TABLE products (id INT PRIMARY KEY, name VARCHAR, price INT, category VARCHAR)").unwrap();

    // 100行挿入
    for i in 1..=100 {
        let cat = if i % 2 == 0 { "electronics" } else { "books" };
        conn.execute(&format!("INSERT INTO products VALUES ({}, 'item_{}', {}, '{}')", i, i, i * 10, cat)).unwrap();
    }

    // information_schema.tables の確認 (approx_row_count が 100)
    let res = conn.query("SELECT table_name, table_rows FROM information_schema.tables WHERE table_name = 'products'").unwrap();
    assert_eq!(res.len(), 1);
    assert_eq!(res[0].get_as::<i64>(1).unwrap(), 100);

    // 10行削除
    conn.execute("DELETE FROM products WHERE id > 90").unwrap();
    let res = conn.query("SELECT table_rows FROM information_schema.tables WHERE table_name = 'products'").unwrap();
    assert_eq!(res[0].get_as::<i64>(0).unwrap(), 90);

    // ANALYZE 実行
    conn.execute("ANALYZE products").unwrap();

    // EXPLAIN コスト出力の確認
    let explain_res = conn.query("EXPLAIN SELECT * FROM products WHERE price > 500").unwrap();
    let plan = explain_res[0].get_as::<String>(0).unwrap();
    assert!(plan.contains("TableScan: products (cost="), "Plan was: {}", plan);
    assert!(plan.contains("rows="), "Plan was: {}", plan);

    // 全体 ANALYZE 実行
    conn.execute("ANALYZE").unwrap();
}

#[test]
fn test_range_scan_index_optimization() {
    let conn = Connection::open_in_memory().unwrap();

    conn.execute("CREATE TABLE metrics (id INT PRIMARY KEY, score INT)").unwrap();
    conn.execute("CREATE INDEX idx_metrics_score ON metrics(score)").unwrap();

    for i in 1..=50 {
        conn.execute(&format!("INSERT INTO metrics VALUES ({}, {})", i, i * 10)).unwrap();
    }

    // Range Scan: score > 400
    let res = conn.query("SELECT id, score FROM metrics WHERE score > 400").unwrap();
    assert_eq!(res.len(), 10);

    // Range Scan: BETWEEN 200 AND 300
    let res_between = conn.query("SELECT id, score FROM metrics WHERE score BETWEEN 200 AND 300").unwrap();
    assert_eq!(res_between.len(), 11);

    // EXPLAIN で IndexScan が選ばれることを確認
    let explain_res = conn.query("EXPLAIN SELECT id, score FROM metrics WHERE score > 400").unwrap();
    let plan = explain_res[0].get_as::<String>(0).unwrap();
    assert!(plan.contains("IndexScan: metrics on index idx_metrics_score"), "Plan was: {}", plan);
}

#[test]
fn test_phase1_max_materialized_rows_limit() {
    let conn = Connection::open_in_memory().unwrap();

    conn.execute("CREATE TABLE big_data (id INT PRIMARY KEY, val INT)").unwrap();
    for i in 1..=20 {
        conn.execute(&format!("INSERT INTO big_data VALUES ({}, {})", i, i)).unwrap();
    }

    // 安全行数上限を 10 行に設定
    conn.execute("SET max_materialized_rows = 10").unwrap();

    // 20行取得しようとするとエラーになる
    let err = conn.query("SELECT * FROM big_data").unwrap_err();
    assert!(err.to_string().contains("Query exceeded maximum materialized row limit"), "Error was: {}", err);

    // LIMIT 5 をつければ正常に取得できる
    let ok_res = conn.query("SELECT * FROM big_data LIMIT 5").unwrap();
    assert_eq!(ok_res.len(), 5);

    // 上限をリセット (100)
    conn.execute("SET max_materialized_rows = 100").unwrap();
    let all_res = conn.query("SELECT * FROM big_data").unwrap();
    assert_eq!(all_res.len(), 20);
}

#[test]
fn test_phase2_buffer_pool_out_of_core() {
    // 8KB Slotted Page と BufferPoolManager の連携
    let disk = Arc::new(DiskManager::new_in_memory());
    let bpm = BufferPoolManager::new(disk, 8); // 8フレームのバッファプール

    // 20枚のページを確保（プールサイズ8を超えるため、ディスク退避とClock置換が発動）
    for page_idx in 0..20 {
        let (page_id, frame_id) = bpm.new_page(0).unwrap();
        assert_eq!(page_id, page_idx as u32);

        {
            let mut frame = bpm.get_frame(frame_id).write();
            let payload = format!("record_data_on_page_{:04}", page_idx);
            SlottedPage::insert_tuple(&mut frame.data, payload.as_bytes()).unwrap();
        }
        bpm.unpin_page(page_id, true).unwrap();
    }

    // 古いページ（ページ 0）を再度フェッチして、Clock置換と読み出しが正常に動作することを確認
    let frame_page0 = bpm.fetch_page(0).unwrap();
    {
        let frame = bpm.get_frame(frame_page0).read();
        let tuple_bytes = SlottedPage::get_tuple(&frame.data, 0).unwrap();
        assert_eq!(tuple_bytes, b"record_data_on_page_0000");
    }
    bpm.unpin_page(0, false).unwrap();
}

#[test]
fn test_phase3_external_merge_sort_and_work_mem() {
    let conn = Connection::open_in_memory().unwrap();

    conn.execute("CREATE TABLE sort_bench (id INT PRIMARY KEY, num INT, payload VARCHAR)").unwrap();

    // 100行挿入
    for i in 1..=100 {
        let rand_num = (i * 37) % 100;
        conn.execute(&format!("INSERT INTO sort_bench VALUES ({}, {}, 'text_payload_{}')", i, rand_num, i)).unwrap();
    }

    // work_mem を極小 (256B) に設定して外部ディスクスピルを強制
    conn.execute("SET work_mem = '256B'").unwrap();

    let res = conn.query("SELECT id, num FROM sort_bench ORDER BY num ASC, id ASC").unwrap();
    assert_eq!(res.len(), 100);

    // ソート順序の正当性を検証
    let mut prev_num = -1;
    for row in res {
        let num: i32 = row.get_as(1).unwrap();
        assert!(num >= prev_num, "Sort violated: {} < {}", num, prev_num);
        prev_num = num;
    }
}

#[test]
fn test_phase3_admission_control_coordinator() {
    let coordinator = MemoryGrantCoordinator::new(100 * 1024); // 100KB

    // 60KB 予約
    let grant1 = coordinator.acquire_grant(60 * 1024, Duration::from_millis(50)).unwrap();
    assert_eq!(coordinator.available_memory(), 40 * 1024);

    // 50KB 予約を試行（残り40KBのため待機タイムアウト）
    let res = coordinator.acquire_grant(50 * 1024, Duration::from_millis(30));
    assert!(res.is_err());

    // grant1 を解放
    drop(grant1);
    assert_eq!(coordinator.available_memory(), 100 * 1024);

    // 再度 50KB 予約を試行（成功）
    let grant2 = coordinator.acquire_grant(50 * 1024, Duration::from_millis(50)).unwrap();
    assert_eq!(coordinator.reserved_memory(), 50 * 1024);
    drop(grant2);
}
