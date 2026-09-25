use std::net::SocketAddr;
use h2::{Connection, H2Result};

#[tokio::main]
async fn main() -> H2Result<()> {
    tracing_subscriber::fmt::init();

    println!("============================================================");
    println!("  H2 Database in Rust - PostgreSQL Wire Protocol Demo Server");
    println!("============================================================");

    // 1. データベースファイルを開く (H2_DB_PATH またはデフォルト psql_demo.h2)
    let db_path = std::env::var("H2_DB_PATH").unwrap_or_else(|_| "psql_demo.h2".to_string());
    let conn = if db_path == ":memory:" {
        println!("[INFO] Opened in-memory database.");
        Connection::open_in_memory()?
    } else {
        println!("[INFO] Opened database at: {}", db_path);
        Connection::open(&db_path)?
    };

    // 2. 初期テーブルとデモデータの準備
    conn.execute(
        "CREATE TABLE IF NOT EXISTS server_nodes (
            id INT PRIMARY KEY,
            hostname VARCHAR(50) NOT NULL,
            ip_address VARCHAR(20) NOT NULL,
            status VARCHAR(20) NOT NULL,
            cpu_usage DOUBLE,
            metadata JSON
        )"
    )?;

    // 既存行数をチェック
    let count_rows = conn.query("SELECT COUNT(*) FROM server_nodes")?;
    let count: i64 = count_rows[0].get_as(0).unwrap_or(0);
    if count == 0 {
        conn.execute(
            "INSERT INTO server_nodes VALUES 
            (1, 'node-tokyo-01', '192.168.1.10', 'ONLINE', 12.5, '{\"zone\": \"ap-northeast-1a\", \"role\": \"primary\"}'),
            (2, 'node-tokyo-02', '192.168.1.11', 'ONLINE', 45.8, '{\"zone\": \"ap-northeast-1c\", \"role\": \"replica\"}'),
            (3, 'node-osaka-01', '192.168.2.10', 'DRAINING', 5.2, '{\"zone\": \"ap-northeast-3a\", \"role\": \"backup\"}')"
        )?;
        println!("[INFO] Initialized demo data (3 server nodes).");
    }

    // 3. PostgreSQL ワイヤプロトコルサーバーを開始 (ポート 5433 または PG_PORT)
    let port = std::env::var("PG_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(5433);
    let bind_addr: SocketAddr = format!("0.0.0.0:{}", port).parse().unwrap();
    let actual_addr = match conn.start_pg_server(bind_addr).await {
        Ok(addr) => addr,
        Err(e) => {
            eprintln!("[WARN] Port {} might be in use ({e}). Trying automatic port...", port);
            let fallback: SocketAddr = "0.0.0.0:0".parse().unwrap();
            conn.start_pg_server(fallback).await?
        }
    };

    println!();
    println!("  🚀 Server is listening on: {}", actual_addr);
    println!("  ----------------------------------------------------------");
    println!("  Connect with psql:");
    println!("    psql -h {} -p {} -U postgres -d mydb", actual_addr.ip(), actual_addr.port());
    println!();
    println!("  Connect with GUI (DBeaver / TablePlus / DataGrip):");
    println!("    Host: {}", actual_addr.ip());
    println!("    Port: {}", actual_addr.port());
    println!("    Database: mydb (any)");
    println!("    Username: postgres (any)");
    println!("    Password: (leave blank)");
    println!("  ----------------------------------------------------------");
    println!("  Press Ctrl+C to stop the server.");
    println!();

    // 4. バックグラウンド非同期同期タスク (1秒間隔で OS キャッシュを fsync)
    let sync_conn = conn.clone();
    let is_memory = db_path == ":memory:";
    let sync_handle = tokio::spawn(async move {
        if is_memory {
            return;
        }
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
        loop {
            interval.tick().await;
            let _ = sync_conn.sync();
        }
    });

    tokio::signal::ctrl_c().await.expect("Failed to listen for Ctrl+C");
    println!("\n[INFO] Flushed data and shutting down PG-Wire demo server. Bye!");
    sync_handle.abort();
    let _ = conn.sync();
    Ok(())
}
