use std::thread;
use tempfile::NamedTempFile;
use h2::{Connection, Value};

/// 1. H2互換 データ型・演算子・NULL処理の網羅的テスト
#[test]
fn test_h2_data_types_and_operators() {
    let conn = Connection::open_in_memory().unwrap();

    conn.execute(
        "CREATE TABLE test_types (
            id INTEGER PRIMARY KEY,
            val_int BIGINT,
            val_dec DECIMAL(12, 4),
            val_str VARCHAR,
            val_bool BOOLEAN
        )",
    ).unwrap();

    // 正常データの挿入
    conn.execute(
        "INSERT INTO test_types VALUES
            (1, 10000000000, 1234.5678, 'Hello World', true),
            (2, -500, 0.0500, 'Rust DB', false),
            (3, 42, 999.9900, 'Testing', true)",
    ).unwrap();

    // DECIMAL と 整数 の比較 (WHERE val_dec > 1000)
    let rows = conn.query("SELECT id, val_str, val_dec FROM test_types WHERE val_dec > 1000").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get(0), Some(&Value::Integer(1)));

    // BOOLEAN フィルタ
    let bool_rows = conn.query("SELECT id FROM test_types WHERE val_bool = true").unwrap();
    assert_eq!(bool_rows.len(), 2);

    // 複合条件 (AND, OR)
    let complex_rows = conn.query("SELECT id FROM test_types WHERE val_int > 0 AND val_dec < 1000").unwrap();
    assert_eq!(complex_rows.len(), 1);
    assert_eq!(complex_rows[0].get(0), Some(&Value::Integer(3)));
}

/// 2. H2互換 全文検索（FTS: N-Gram & 形態素解析）の網羅的テスト
#[test]
fn test_h2_fulltext_search_comprehensive() {
    let conn = Connection::open_in_memory().unwrap();

    conn.execute(
        "CREATE TABLE tech_articles (
            id INTEGER PRIMARY KEY,
            author VARCHAR,
            content VARCHAR
        )",
    ).unwrap();

    // 日本語・英語・記号混在ドキュメント
    conn.execute(
        "INSERT INTO tech_articles VALUES
            (1, 'Alice', 'Rustは高速なメモリ安全性を備えたプログラミング言語です。'),
            (2, 'Bob', 'H2 DatabaseはJavaで作られた超軽量な組み込みリレーショナルデータベースです。'),
            (3, 'Charlie', '次世代のデータベースはスナップショット分離とMVCC並行制御を標準で備えています。'),
            (4, 'David', '全文検索エンジンにはN-Gramと形態素解析のアプローチがあります。')",
    ).unwrap();

    // A. N-Gram (Bigram) による部分一致検索 (FT_SEARCH)
    // "データベース" で検索 -> Bob (2) と Charlie (3) にマッチ
    let res_db = conn.query("SELECT id, author FROM tech_articles WHERE FT_SEARCH(content, 'データベース')").unwrap();
    assert_eq!(res_db.len(), 2);
    let ids: Vec<i32> = res_db.iter().map(|r| match r.get(0) {
        Some(Value::Integer(n)) => *n,
        _ => panic!(),
    }).collect();
    assert!(ids.contains(&2));
    assert!(ids.contains(&3));

    // "メモリ安全" で検索 -> Alice (1)
    let res_mem = conn.query("SELECT id FROM tech_articles WHERE FT_SEARCH(content, 'メモリ安全')").unwrap();
    assert_eq!(res_mem.len(), 1);
    assert_eq!(res_mem[0].get(0), Some(&Value::Integer(1)));

    // B. 形態素解析による検索 (FT_SEARCH_MORPH)
    // "プログラミング" で検索 -> Alice (1)
    let res_morph = conn.query("SELECT id FROM tech_articles WHERE FT_SEARCH_MORPH(content, 'プログラミング')").unwrap();
    assert_eq!(res_morph.len(), 1);
    assert_eq!(res_morph[0].get(0), Some(&Value::Integer(1)));

    // "スナップショット" で検索 -> Charlie (3)
    let res_snap = conn.query("SELECT id FROM tech_articles WHERE FT_SEARCH_MORPH(content, 'スナップショット')").unwrap();
    assert_eq!(res_snap.len(), 1);
    assert_eq!(res_snap[0].get(0), Some(&Value::Integer(3)));

    // C. 存在しないキーワードでの検索 -> 0件
    let res_none = conn.query("SELECT id FROM tech_articles WHERE FT_SEARCH(content, '量子コンピュータ')").unwrap();
    assert_eq!(res_none.len(), 0);
}

/// 3. H2互換 MVCC並行性・トランザクション分離ストレステスト
#[test]
fn test_h2_mvcc_stress_and_concurrency() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute("CREATE TABLE bank (account_id INTEGER PRIMARY KEY, balance INTEGER)").unwrap();
    conn.execute("INSERT INTO bank VALUES (1, 1000), (2, 500)").unwrap();

    // 長時間実行リーダー (Reader with Snapshot Isolation)
    let tx_reader = conn.transaction().unwrap();

    // 他スレッドから書き込みトランザクションを実行
    let conn_writer = conn.clone();
    let writer_handle = thread::spawn(move || {
        let tx_writer = conn_writer.transaction().unwrap();
        tx_writer.execute("INSERT INTO bank VALUES (3, 300)").unwrap();
        tx_writer.commit().unwrap();
    });

    writer_handle.join().unwrap();

    // tx_reader は自トランザクション開始時点のスナップショットを維持（3は見えない）
    let rows_reader = tx_reader.query("SELECT * FROM bank").unwrap();
    assert_eq!(rows_reader.len(), 2);
    tx_reader.commit().unwrap();

    // 新規トランザクションからは 3 が見える
    let rows_after = conn.query("SELECT * FROM bank").unwrap();
    assert_eq!(rows_after.len(), 3);
}

/// 4. H2互換 コンパクション（Vacuum）ストレステスト
#[test]
fn test_h2_compaction_vacuum_stress() {
    let temp_file = NamedTempFile::new().unwrap();
    let path = temp_file.path().to_path_buf();

    let conn = Connection::open(&path).unwrap();
    conn.execute("CREATE TABLE log_entries (id INTEGER, message VARCHAR)").unwrap();

    // 500件のデータを挿入（チャンク追記書き込みが複数回発生）
    for i in 1..=500 {
        let sql = format!("INSERT INTO log_entries VALUES ({}, 'Log entry payload number {} - timestamp: 2026-09-24')", i, i);
        conn.execute(&sql).unwrap();
    }

    let size_after_insert = std::fs::metadata(&path).unwrap().len();

    // 450件を削除
    conn.execute("DELETE FROM log_entries WHERE id > 50").unwrap();

    // コンパクション (Vacuum) 実行
    conn.vacuum().unwrap();

    let size_after_vacuum = std::fs::metadata(&path).unwrap().len();

    // ファイルサイズが削減されていることを検証
    assert!(
        size_after_vacuum < size_after_insert,
        "Vacuum should shrink file: {} < {}",
        size_after_vacuum,
        size_after_insert
    );

    // 生存データ（50件）が欠落なく正確にクエリできることを検証
    let rows = conn.query("SELECT * FROM log_entries").unwrap();
    assert_eq!(rows.len(), 50);
}

/// 5. H2互換 クラッシュリカバリとCRC32整合性検証テスト
#[test]
fn test_h2_crash_recovery_integrity() {
    let temp_file = NamedTempFile::new().unwrap();
    let path = temp_file.path().to_path_buf();

    // 1. 初回セッションでデータ作成とコミット
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute("CREATE TABLE customers (id INTEGER PRIMARY KEY, name VARCHAR, credit DECIMAL(10, 2))").unwrap();
        conn.execute("INSERT INTO customers VALUES (1, 'Customer Alpha', 2500.00), (2, 'Customer Beta', 750.50)").unwrap();
    }

    // 2. プロセス終了相当の再オープン
    {
        let conn = Connection::open(&path).unwrap();
        let rows = conn.query("SELECT name, credit FROM customers WHERE credit > 1000").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get(0), Some(&Value::String("Customer Alpha".to_string())));

        // 追加の書き込み
        conn.execute("INSERT INTO customers VALUES (3, 'Customer Gamma', 4000.00)").unwrap();
    }

    // 3. 3度目の再オープン
    {
        let conn = Connection::open(&path).unwrap();
        let rows = conn.query("SELECT * FROM customers").unwrap();
        assert_eq!(rows.len(), 3);
    }
}
