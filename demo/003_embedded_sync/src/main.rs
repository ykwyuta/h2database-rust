use std::str::FromStr;
use rust_decimal::Decimal;
use uuid::Uuid;
use h2::{params, Connection, H2Result};

fn main() -> H2Result<()> {
    println!("============================================================");
    println!("  H2 Database in Rust - Synchronous Embedded Demo");
    println!("============================================================");

    // 1. ファイルベースでデータベースを開く
    let db_path = "embedded_sync.h2";
    let conn = Connection::open(db_path)?;
    println!("[1] Opened database at: {db_path}");

    // 2. テーブルとセカンダリインデックスの作成 (DDL)
    conn.execute(
        "CREATE TABLE IF NOT EXISTS articles (
            id INT PRIMARY KEY,
            uuid_key UUID NOT NULL,
            title VARCHAR(150) NOT NULL,
            views INT NOT NULL,
            price DECIMAL(10, 2) NOT NULL,
            tags JSON,
            content TEXT,
            published BOOLEAN NOT NULL
        )"
    )?;

    conn.execute("CREATE INDEX IF NOT EXISTS idx_art_views ON articles (views)")?;
    println!("[2] Created table 'articles' and secondary index 'idx_art_views'.");

    // 3. パラメータ付き INSERT (params! マクロ)
    let uid1 = Uuid::new_v4();
    let uid2 = Uuid::new_v4();

    conn.execute_params(
        "INSERT INTO articles VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        &params![
            1,
            uid1,
            "Rustで始めるデータベースプログラミング",
            1500,
            Decimal::from_str("29.80").unwrap(),
            serde_json::json!(["Rust", "Database", "Backend"]),
            "本記事ではCopy-on-Write B-TreeによるMVStoreの設計とMVCCスナップショット分離を徹底解説します。",
            true,
        ],
    )?;

    conn.execute_params(
        "INSERT INTO articles VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        &params![
            2,
            uid2,
            "非同期Tokioによるハイブリッドアーキテクチャ",
            850,
            Decimal::from_str("19.50").unwrap(),
            serde_json::json!(["Tokio", "Async", "Network"]),
            "組み込み実行とPostgreSQLワイヤプロトコルのハイブリッドサーバーを実現する手法を紹介します。",
            true,
        ],
    )?;
    println!("[3] Inserted sample articles with parameters.");

    // 4. 型安全なクエリ結果の走査 (Row::get_as)
    println!("\n[4] Querying articles ordered by views:");
    let rows = conn.query("SELECT id, title, views, price, published FROM articles ORDER BY views DESC")?;
    for row in rows {
        let id: i32 = row.get_as(0)?;
        let title: String = row.get_as(1)?;
        let views: i32 = row.get_as(2)?;
        let price: Decimal = row.get_as(3)?;
        let is_pub: bool = row.get_as(4)?;
        println!("  - #{id} {title} | Views: {views}, Price: ${price}, Published: {is_pub}");
    }

    // 5. 日本語全文検索 (FTS: N-Gram & 形態素境界)
    println!("\n[5] Full-Text Search (Japanese FTS):");
    let fts_rows = conn.query("SELECT id, title FROM articles WHERE FT_SEARCH(content, 'スナップショット')")?;
    for r in fts_rows {
        let title: String = r.get_as(1)?;
        println!("  [FTS Hit] {title}");
    }

    // 6. JSON 抽出演算子 (->, ->>)
    println!("\n[6] JSON Extraction (->>):");
    let json_rows = conn.query("SELECT id, title, tags ->> 0 AS first_tag FROM articles")?;
    for r in json_rows {
        let title: String = r.get_as(1)?;
        let first_tag: Option<String> = r.get_as(2)?;
        println!("  - {title} (Primary Tag: {first_tag:?})");
    }

    // 7. トランザクション処理 (スナップショット分離 & 自動ロールバック)
    println!("\n[7] Transaction with Commit & Rollback:");
    {
        // ロールバックの検証
        let tx = conn.transaction()?;
        tx.execute("UPDATE articles SET views = views + 10000 WHERE id = 1")?;
        println!("  Inside tx: views incremented by 10000.");
        tx.rollback()?;
        println!("  Rolled back tx.");

        let check_row = conn.query("SELECT views FROM articles WHERE id = 1")?;
        let v: i32 = check_row[0].get_as(0)?;
        println!("  After rollback: views is restored to: {v}");
    }

    {
        // コミットの検証
        let tx = conn.transaction()?;
        tx.execute("UPDATE articles SET views = views + 50 WHERE id = 1")?;
        tx.commit()?;
        println!("  Committed tx.");

        let check_row = conn.query("SELECT views FROM articles WHERE id = 1")?;
        let v: i32 = check_row[0].get_as(0)?;
        println!("  After commit: views updated to: {v}");
    }

    // 8. ストレージのコンパクション (Vacuum)
    println!("\n[8] Running Vacuum compaction to reclaim space...");
    conn.vacuum()?;
    println!("  Vacuum complete! Current version: {}", conn.version());

    println!("\n[SUCCESS] Synchronous embedded demo finished cleanly!");
    Ok(())
}
