use std::net::SocketAddr;
use h2::{Connection, H2Result};

#[tokio::main]
async fn main() -> H2Result<()> {
    tracing_subscriber::fmt::init();

    println!("============================================================");
    println!("  H2 Database in Rust - PostgreSQL Wire Protocol Demo Server");
    println!("============================================================");

    // 1. データベースファイルを開く
    let db_path = "psql_demo.h2";
    let conn = Connection::open(db_path)?;
    println!("[INFO] Opened database at: {}", db_path);

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

    // 3. PostgreSQL ワイヤプロトコルサーバーを開始 (ポート 5432)
    let bind_addr: SocketAddr = "127.0.0.1:5432".parse().unwrap();
    let actual_addr = match conn.start_pg_server(bind_addr).await {
        Ok(addr) => addr,
        Err(e) => {
            eprintln!("[WARN] Port 5432 might be in use ({e}). Trying automatic port...");
            let fallback: SocketAddr = "127.0.0.1:0".parse().unwrap();
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

    tokio::signal::ctrl_c().await.expect("Failed to listen for Ctrl+C");
    println!("\n[INFO] Shutting down PG-Wire demo server. Bye!");
    Ok(())
}
