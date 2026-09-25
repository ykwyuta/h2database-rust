use std::time::Instant;
use h2::storage::{DecoupledCluster, NodeState};
use h2::H2Result;

fn main() -> H2Result<()> {
    println!("============================================================");
    println!("  H2 Database in Rust - Decoupled Storage Cluster Demo");
    println!("  (AWS Aurora Model: 4 of 6 Quorum across 3 AZs)");
    println!("============================================================");

    // 1. 6ノード分散ストレージフリート (4 of 6 Quorum, 3 AZ) ＋ 2台のリードレプリカでクラスタ起動
    let mut cluster = DecoupledCluster::new_6nodes("aurora-enterprise", 2)?;
    println!("[1] Decoupled Aurora Cluster initialized:");
    println!("    Storage Fleet : 6 Smart Storage Nodes (2 in AZ-1, 2 in AZ-2, 2 in AZ-3)");
    println!("    Quorum Config : 4 of 6 Quorum (Tolerates 2 node / 1 full AZ failure)");
    println!("    Compute Fleet : 1 Primary (RW) + 2 Shared-Storage Read Replicas (RO)");

    let primary_conn = cluster.primary_connection();
    let replica1_conn = cluster.replica_connection(0)?;
    let replica2_conn = cluster.replica_connection(1)?;

    // 2. Primary でテーブル作成 & INSERT ("The Log is the Database")
    // ダーティページは転送せず、WAL ログレコードのみを 6 台のストレージノードへ並行送出
    println!("\n[2] Primary executing write operations (WAL records sent concurrently):");
    let start_write = Instant::now();
    primary_conn.execute("CREATE TABLE fleet_vehicles (vin VARCHAR PRIMARY KEY, model VARCHAR, battery_pct INT);")?;
    primary_conn.execute("INSERT INTO fleet_vehicles VALUES ('VIN-1001', 'CyberTruck', 95);")?;
    primary_conn.execute("INSERT INTO fleet_vehicles VALUES ('VIN-1002', 'Model Y', 80);")?;
    let write_elapsed = start_write.elapsed();
    println!("    Writes committed to Quorum in: {:?}", write_elapsed);

    // 3. ゼロストレージ・リードレプリカでの参照 (共有ストレージからオンデマンド読み出し)
    println!("\n[3] Reading from Read Replica 1 & 2 (Zero extra storage cost!):");
    let r1_rows = replica1_conn.query("SELECT vin, model, battery_pct FROM fleet_vehicles ORDER BY vin;")?;
    for row in r1_rows {
        let vin: String = row.get_as(0)?;
        let model: String = row.get_as(1)?;
        let bat: i32 = row.get_as(2)?;
        println!("    [Replica 1] Vehicle: {vin} ({model}) - Battery: {bat}%");
    }

    let r2_rows = replica2_conn.query("SELECT COUNT(*) FROM fleet_vehicles;")?;
    println!("    [Replica 2] Total vehicle count: {}", r2_rows[0].get_as::<i64>(0)?);

    // 4. 耐障害性シミュレーション: AZ-1 の 2 ノードが完全停止 (1 AZ 完全喪失)
    println!("\n[4] Simulating Full AZ-1 Failure (Storage Node 1 & Node 2 OFFLINE):");
    cluster.fleet().nodes()[0].set_state(NodeState::Offline);
    cluster.fleet().nodes()[1].set_state(NodeState::Offline);
    println!("    AZ-1 (Node 1, Node 2) is down!");

    // 残り 4 ノード (AZ-2, AZ-3) で 4 of 6 クォーラムが成立するため、無停止でサービス継続！
    println!("    Primary writing with 4 of 6 remaining nodes...");
    primary_conn.execute("INSERT INTO fleet_vehicles VALUES ('VIN-1003', 'Roadster', 100);")?;
    println!("    Write SUCCEEDED without interruption!");

    let updated_count = replica1_conn.query("SELECT COUNT(*) FROM fleet_vehicles;")?;
    println!("    Replica reading after AZ failure: count = {}", updated_count[0].get_as::<i64>(0)?);

    // 5. ゴシップ自己修復 (Peer Gossip Self-Healing)
    println!("\n[5] Healing AZ-1: Bringing Node 1 & Node 2 back ONLINE:");
    cluster.fleet().nodes()[0].set_state(NodeState::Online);
    cluster.fleet().nodes()[1].set_state(NodeState::Online);

    let healed_logs = cluster.step_gossip_repair();
    println!("    Gossip Repair Worker synchronized {healed_logs} missing logs to restored nodes!");
    println!("    Node 1 Flushed LSN: {}", cluster.fleet().nodes()[0].flushed_lsn());
    println!("    Node 3 Flushed LSN: {}", cluster.fleet().nodes()[2].flushed_lsn());

    // 6. 瞬間フェイルオーバー (Promote Replica to New Primary)
    println!("\n[6] Performing Instant Failover (Replica 1 -> New Primary):");
    let old_primary_conn = primary_conn;
    let new_token = cluster.failover_to_replica(0)?;
    println!("    Promoted Replica 1 to Primary with Fencing Token (Epoch: {})", new_token.val());
    println!("    (No Redo log roll-forward needed! Storage fleet is already consistent)");

    let new_primary_conn = cluster.primary_connection();
    new_primary_conn.execute("INSERT INTO fleet_vehicles VALUES ('VIN-1004', 'Semi Truck', 88);")?;
    println!("    New Primary successfully executed INSERT.");

    // 7. スプリットブレイン防止 (Split-Brain Prevention)
    println!("\n[7] Verifying Fencing Token Guard (Old Primary Stale Packet):");
    let zombie_write = old_primary_conn.execute("INSERT INTO fleet_vehicles VALUES ('VIN-9999', 'Ghost Car', 0);");
    match zombie_write {
        Err(e) => {
            println!("    [Split-Brain Prevented] Stale primary write was safely rejected: {:?}", e);
        }
        Ok(_) => panic!("Stale primary write should be rejected!"),
    }

    println!("\n[SUCCESS] Decoupled Aurora storage cluster demo finished cleanly!");
    Ok(())
}
