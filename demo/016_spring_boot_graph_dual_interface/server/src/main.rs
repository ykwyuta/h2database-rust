use std::net::SocketAddr;
use h2::{Connection, H2Result};

#[tokio::main]
async fn main() -> H2Result<()> {
    tracing_subscriber::fmt::init();

    println!("================================================================================");
    println!("  🚀 H2 Database Rust - Dual-Protocol Graph Server (Bolt + PGWire/SQL)");
    println!("================================================================================");

    // 1. 共有データベース（MVStore）のオープン
    let conn = Connection::open_in_memory()?;
    println!("[INFO] Storage engine initialized (MVCC in-memory store).");

    // 2. リレーショナルテーブルの初期化（SQL側とのハイブリッドJOIN用）
    conn.execute(
        "CREATE TABLE departments (
            id INTEGER PRIMARY KEY,
            name VARCHAR NOT NULL,
            budget DECIMAL(12, 2),
            location VARCHAR
        )",
    )?;
    conn.execute(
        "INSERT INTO departments VALUES
            (1, 'Engineering', 2500000.00, 'Tokyo HQ'),
            (2, 'Sales & Marketing', 1800000.00, 'Osaka Branch'),
            (3, 'Advanced Research', 3200000.00, 'Tsukuba Lab')",
    )?;
    println!("[INFO] Relational table 'departments' initialized with 3 records.");

    // 3. PostgreSQL 互換ワイヤプロトコルサーバーの起動 (SQL / JDBC 用)
    let pg_addr: SocketAddr = "127.0.0.1:5432".parse().unwrap();
    let pg_bound = conn.start_pg_server(pg_addr).await?;
    println!("  [SQL Interface]     PostgreSQL Wire listening on: {}", pg_bound);

    // 4. Neo4j 互換 Bolt プロトコルサーバーの起動 (Cypher / Bolt 用)
    let bolt_addr: SocketAddr = "127.0.0.1:7687".parse().unwrap();
    let bolt_bound = conn.start_bolt_server(bolt_addr, "company").await?;
    println!("  [Bolt Interface]    Neo4j Bolt Protocol listening on: {}", bolt_bound);

    println!("--------------------------------------------------------------------------------");
    println!("  Spring Boot application can now connect via:");
    println!("    [Interface 1] Neo4j Java Driver (Bolt) -> bolt://localhost:7687");
    println!("    [Interface 2] PostgreSQL JDBC (SQL)    -> jdbc:postgresql://localhost:5432/company_db");
    println!("--------------------------------------------------------------------------------");
    println!("  Press Ctrl+C to terminate the server.");

    tokio::signal::ctrl_c().await.expect("Failed to listen for Ctrl+C");
    println!("\n[INFO] Shutting down demo graph server. Goodbye!");
    Ok(())
}
