use std::time::Duration;
use h2::replication::{Instance, InstanceConfig, InstanceRole, SyncReplicationMode};
use h2::{H2Error, H2Result};

fn main() -> H2Result<()> {
    println!("============================================================");
    println!("  H2 Database in Rust - Synchronous Replication Demo");
    println!("  (PostgreSQL synchronous_commit = remote_apply Model)");
    println!("============================================================");

    // 1. Primary インスタンス (Read-Write) の起動
    // ポート 0 を指定して OS による動的ポート割り当てを使用
    let primary_config = InstanceConfig {
        role: InstanceRole::Primary {
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            sync_mode: SyncReplicationMode::RemoteApply,
            apply_timeout: Duration::from_secs(5),
        },
    };
    let primary = Instance::open_in_memory(primary_config)?;
    let primary_addr = primary.replication_addr().unwrap();
    println!("[1] Primary instance started (Read-Write) at: {primary_addr}");

    // 2. Standby インスタンス (Read-Only) の起動 & Primary への接続
    let standby_config = InstanceConfig::standby(primary_addr);
    let standby = Instance::open_in_memory(standby_config)?;
    println!("[2] Standby instance connected and synced initial snapshot.");
    println!("    Primary is_read_only: {}", primary.is_read_only());
    println!("    Standby is_read_only: {}", standby.is_read_only());

    let p_conn = primary.connect()?;
    let s_conn = standby.connect()?;

    // 3. Primary でテーブル作成 & INSERT
    println!("\n[3] Primary executing DDL and INSERT...");
    p_conn.execute("CREATE TABLE accounts (id INT PRIMARY KEY, owner VARCHAR, balance INT);")?;
    p_conn.execute("INSERT INTO accounts VALUES (1, 'Alice', 1000);")?;
    p_conn.execute("INSERT INTO accounts VALUES (2, 'Bob', 2500);")?;
    println!("    Primary commit confirmed! (remote_apply ensured standby has applied the changes)");

    // 4. Standby での即時可視性確認
    println!("\n[4] Querying Standby (Read-Only replica):");
    let rows = s_conn.query("SELECT id, owner, balance FROM accounts ORDER BY id;")?;
    for row in rows {
        let id: i32 = row.get_as(0)?;
        let owner: String = row.get_as(1)?;
        let balance: i32 = row.get_as(2)?;
        println!("    [Standby] Account #{id}: {owner} -> Balance: {balance} JPY");
    }

    // 5. Standby での書き込み禁止ガードの検証
    println!("\n[5] Verifying Read-Only guard on Standby:");
    let write_res = s_conn.execute("INSERT INTO accounts VALUES (3, 'Mallory', 9999);");
    match write_res {
        Err(H2Error::ReadOnly(msg)) => {
            println!("    [Guard OK] Write rejected on standby with ReadOnly error: {msg}");
        }
        other => panic!("Unexpected result on standby write: {:?}", other),
    }

    // 6. Primary で明示的トランザクションを実行
    println!("\n[6] Primary executing explicit transaction (Alice sends 200 JPY to Bob):");
    p_conn.execute("BEGIN;")?;
    p_conn.execute("UPDATE accounts SET balance = balance - 200 WHERE id = 1;")?;
    p_conn.execute("UPDATE accounts SET balance = balance + 200 WHERE id = 2;")?;
    p_conn.execute("COMMIT;")?;
    println!("    Transaction committed on primary.");

    // Standby で残高を確認
    println!("\n[7] Standby reading updated balances:");
    let updated_rows = s_conn.query("SELECT id, owner, balance FROM accounts ORDER BY id;")?;
    for row in updated_rows {
        let id: i32 = row.get_as(0)?;
        let owner: String = row.get_as(1)?;
        let balance: i32 = row.get_as(2)?;
        println!("    [Standby] Account #{id}: {owner} -> Balance: {balance} JPY");
    }

    println!("\n[SUCCESS] Synchronous replication demo completed cleanly!");
    Ok(())
}
