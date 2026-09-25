use std::net::SocketAddr;
use std::time::Duration;
use h2::replication::{Instance, InstanceConfig, InstanceRole, SyncReplicationMode};
use h2::H2Result;

#[tokio::main]
async fn main() -> H2Result<()> {
    tracing_subscriber::fmt::init();

    println!("================================================================================");
    println!("  🚀 H2 Database Rust - Dual-Node Replication Server for Spring Boot Demo");
    println!("================================================================================");

    // 1. Primary (Read-Write) インスタンスのセットアップ
    let primary_config = InstanceConfig {
        role: InstanceRole::Primary {
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            sync_mode: SyncReplicationMode::RemoteApply,
            apply_timeout: Duration::from_secs(5),
        },
    };
    let primary = Instance::open_in_memory(primary_config)?;
    let repl_addr = primary.replication_addr().unwrap();
    println!("[INFO] Primary instance started (Role: ReadWrite, Replication Hub: {repl_addr}).");

    // 2. Standby (Read-Only) インスタンスのセットアップ & レプリケーション接続
    let standby_config = InstanceConfig::standby(repl_addr);
    let standby = Instance::open_in_memory(standby_config)?;
    println!("[INFO] Standby instance started (Role: ReadOnly, Synchronized with Primary).");

    let p_conn = primary.connect()?;
    let s_conn = standby.connect()?;

    // 3. Primary PGWire サーバーの起動 (Port: 5432)
    let primary_addr: SocketAddr = "127.0.0.1:5432".parse().unwrap();
    let primary_bound = p_conn.start_pg_server(primary_addr).await?;
    println!("  [Node 1] Primary (Read-Write) listening on: {}", primary_bound);

    // 4. Standby PGWire サーバーの起動 (Port: 5433)
    let standby_addr: SocketAddr = "127.0.0.1:5433".parse().unwrap();
    let standby_bound = s_conn.start_pg_server(standby_addr).await?;
    println!("  [Node 2] Standby (Read-Only)  listening on: {}", standby_bound);

    println!("--------------------------------------------------------------------------------");
    println!("  Spring Boot application can now connect via:");
    println!("    Primary (Read-Write) -> jdbc:postgresql://localhost:5432/primary_db");
    println!("    Standby (Read-Only)  -> jdbc:postgresql://localhost:5433/standby_db");
    println!("--------------------------------------------------------------------------------");
    println!("  Press Ctrl+C to terminate both servers.");

    tokio::signal::ctrl_c().await.expect("Failed to listen for Ctrl+C");
    println!("\n[INFO] Shutting down demo servers. Goodbye!");
    Ok(())
}
