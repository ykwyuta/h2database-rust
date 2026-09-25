use std::sync::Arc;
use std::time::{Duration, Instant};

use h2::storage::{DecoupledCluster, NodeState, QuorumConfig, SmartStorageNode, StorageFleet};
use h2::{FencingToken, H2Error, LogRecord};

#[test]
fn test_quorum_write_2_of_3_basic() {
    let cluster = DecoupledCluster::new_3nodes("test_q3", 1).unwrap();

    // 1. Primary で SQL テーブル作成 & データ挿入
    let primary_conn = cluster.primary_connection();
    primary_conn.execute("CREATE TABLE products (id INT PRIMARY KEY, name VARCHAR, price DECIMAL);").unwrap();
    primary_conn.execute("INSERT INTO products VALUES (1, 'Rust Book', 4500.00);").unwrap();
    primary_conn.execute("INSERT INTO products VALUES (2, 'Storage Guide', 3200.00);").unwrap();

    // 2. ストレージフリートの各ノードに WAL ログが保存されていることを確認
    let nodes = cluster.fleet().nodes();
    let mut acked_nodes = 0;
    for node in nodes {
        if node.flushed_lsn() > 0 {
            acked_nodes += 1;
        }
    }
    // 2 of 3 クォーラム以上で永続化されていること
    assert!(acked_nodes >= 2, "At least 2 nodes must have written the logs");

    // 3. バックグラウンドマテリアライズを実行
    let applied_count = cluster.step_materialization();
    assert!(applied_count > 0, "Materializer should apply pending WAL logs");

    // 4. リードレプリカから最新データが参照可能であることを確認
    let replica_conn = cluster.replica_connection(0).unwrap();
    let rows = replica_conn.query("SELECT id, name, price FROM products ORDER BY id;").unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].get_as::<i32>(0).unwrap(), 1);
    assert_eq!(rows[0].get_as::<String>(1).unwrap(), "Rust Book");
    assert_eq!(rows[1].get_as::<i32>(0).unwrap(), 2);
    assert_eq!(rows[1].get_as::<String>(1).unwrap(), "Storage Guide");
}

#[test]
fn test_tail_latency_elimination() {
    // 3ノード構成
    let nodes = vec![
        Arc::new(SmartStorageNode::new(1, "az-1")),
        Arc::new(SmartStorageNode::new(2, "az-2")),
        Arc::new(SmartStorageNode::new(3, "az-3")),
    ];

    // ノード3に 300ms の高レイテンシを注入 (Slow Disk / Lagging Network)
    nodes[2].set_simulated_delay(300);

    let config = QuorumConfig::three_nodes();
    let fleet = Arc::new(StorageFleet::new(nodes, config).unwrap());

    let record = LogRecord::put(100, 99, 1, "test_map", b"key1".to_vec(), b"val1".to_vec());

    let start = Instant::now();
    let acked_lsn = fleet.append_logs_quorum(&[record], FencingToken(1)).unwrap();
    let elapsed = start.elapsed();

    assert_eq!(acked_lsn, 100);
    // 2ノードが即座に応答するため、300ms のノード3を待たずに 150ms 未満で完了する
    assert!(
        elapsed < Duration::from_millis(150),
        "Quorum write should finish quickly without waiting for slow node 3 (took: {:?})",
        elapsed
    );
}

#[test]
fn test_fault_tolerance_1_node_down() {
    let cluster = DecoupledCluster::new_3nodes("test_ft", 1).unwrap();

    // ノード2を Offline (障害発生) に設定
    cluster.fleet().nodes()[1].set_state(NodeState::Offline);

    let primary_conn = cluster.primary_connection();
    // 1ノード停止中でも 2 of 3 クォーラムで書き込みが継続できる
    primary_conn.execute("CREATE TABLE orders (order_id INT PRIMARY KEY, amount INT);").unwrap();
    primary_conn.execute("INSERT INTO orders VALUES (101, 500);").unwrap();

    // レプリカでの読み取りも可能
    let replica_conn = cluster.replica_connection(0).unwrap();
    let rows = replica_conn.query("SELECT * FROM orders WHERE order_id = 101;").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get_as::<i32>(0).unwrap(), 101);
}

#[test]
fn test_peer_gossip_self_healing() {
    let cluster = DecoupledCluster::new_3nodes("test_gossip", 1).unwrap();

    // ノード3 を一時的に Offline にする
    let node3 = Arc::clone(&cluster.fleet().nodes()[2]);
    node3.set_state(NodeState::Offline);

    // Primary からデータを書き込む (ノード1 & ノード2 のみで Quorum 達成)
    let primary_conn = cluster.primary_connection();
    primary_conn.execute("CREATE TABLE inventory (sku VARCHAR PRIMARY KEY, qty INT);").unwrap();
    primary_conn.execute("INSERT INTO inventory VALUES ('ITEM_A', 50);").unwrap();
    primary_conn.execute("INSERT INTO inventory VALUES ('ITEM_B', 100);").unwrap();

    let node1_lsn = cluster.fleet().nodes()[0].flushed_lsn();
    let node3_lsn_before = node3.flushed_lsn();
    assert!(node3_lsn_before < node1_lsn, "Node 3 must be lagging behind while offline");

    // ノード3 がネットワーク復旧 (Online)
    node3.set_state(NodeState::Online);

    // ゴシップ自己修復ワーカーを実行
    let healed = cluster.step_gossip_repair();
    assert!(healed > 0, "Gossip repair should synchronize missing logs to Node 3");

    // ノード3 の LSN がピアに追いついていることを検証
    let node3_lsn_after = node3.flushed_lsn();
    assert_eq!(node3_lsn_after, node1_lsn, "Node 3 should catch up with peer LSN");
}

#[test]
fn test_aurora_6_nodes_enterprise_quorum() {
    // 6ノード構成 (4 of 6 Quorum: 3 AZ x 2ノード)
    let cluster = DecoupledCluster::new_6nodes("aurora_prod", 2).unwrap();
    assert_eq!(cluster.fleet().config().total_nodes, 6);
    assert_eq!(cluster.fleet().config().write_quorum, 4);

    let primary_conn = cluster.primary_connection();
    primary_conn.execute("CREATE TABLE users (id INT PRIMARY KEY, name VARCHAR);").unwrap();
    primary_conn.execute("INSERT INTO users VALUES (1, 'Alice');").unwrap();

    // 2ノード (AZ-1 の全ノード: ノード1とノード2) を完全停止させる (1 AZ 障害)
    cluster.fleet().nodes()[0].set_state(NodeState::Offline);
    cluster.fleet().nodes()[1].set_state(NodeState::Offline);

    // 残り 4 ノード (AZ-2, AZ-3) で 4 of 6 クォーラムが成立し、無停止でサービス継続！
    primary_conn.execute("INSERT INTO users VALUES (2, 'Bob');").unwrap();

    let rows = primary_conn.query("SELECT COUNT(*) FROM users;").unwrap();
    assert_eq!(rows[0].get_as::<i64>(0).unwrap(), 2);

    // さらに 1 ノード落として計 3 ノード停止（過半数喪失）させると、クォーラム不成立になる
    cluster.fleet().nodes()[2].set_state(NodeState::Offline);
    let res = primary_conn.execute("INSERT INTO users VALUES (3, 'Charlie');");
    assert!(res.is_err(), "Write must fail when quorum (4 of 6) cannot be achieved");
}

#[test]
fn test_read_replica_read_only_and_invalidation() {
    let cluster = DecoupledCluster::new_3nodes("test_replica", 2).unwrap();
    let primary_conn = cluster.primary_connection();
    let replica1_conn = cluster.replica_connection(0).unwrap();
    let replica2_conn = cluster.replica_connection(1).unwrap();

    // 1. Primary でテーブル作成とデータ登録
    primary_conn.execute("CREATE TABLE sensors (id INT PRIMARY KEY, temp DOUBLE);").unwrap();
    primary_conn.execute("INSERT INTO sensors VALUES (1, 23.5);").unwrap();

    // 2. Replica 1, 2 で即時参照可能
    let r1_rows = replica1_conn.query("SELECT temp FROM sensors WHERE id = 1;").unwrap();
    assert_eq!(r1_rows[0].get_as::<f64>(0).unwrap(), 23.5);

    let r2_rows = replica2_conn.query("SELECT temp FROM sensors WHERE id = 1;").unwrap();
    assert_eq!(r2_rows[0].get_as::<f64>(0).unwrap(), 23.5);

    // 3. Read Replica への直接書き込みは ReadOnly エラーで拒否される
    let write_err = replica1_conn.execute("INSERT INTO sensors VALUES (2, 26.0);");
    assert!(write_err.is_err());
    match write_err {
        Err(H2Error::ReadOnly(_)) => {} // Expected
        other => panic!("Expected ReadOnly error, got {:?}", other),
    }

    // 4. Primary でデータを更新すると、レプリカへ無効化が伝播し、最新値が見える
    primary_conn.execute("UPDATE sensors SET temp = 28.0 WHERE id = 1;").unwrap();
    let updated_rows = replica1_conn.query("SELECT temp FROM sensors WHERE id = 1;").unwrap();
    assert_eq!(updated_rows[0].get_as::<f64>(0).unwrap(), 28.0);
}

#[test]
fn test_instant_failover_and_fencing_token() {
    let mut cluster = DecoupledCluster::new_3nodes("test_failover", 1).unwrap();
    let old_primary_conn = cluster.primary_connection();

    old_primary_conn.execute("CREATE TABLE cluster_state (epoch INT PRIMARY KEY, leader VARCHAR);").unwrap();
    old_primary_conn.execute("INSERT INTO cluster_state VALUES (1, 'primary-node-1');").unwrap();

    let old_token = cluster.primary().fencing_token();
    assert_eq!(old_token.0, 1);

    // フェイルオーバーを実行: レプリカ 0 を新 Primary に昇格！
    let new_token = cluster.failover_to_replica(0).unwrap();
    assert_eq!(new_token.0, 2, "New leader must acquire bumped fencing token (epoch 2)");

    // 新 Primary での書き込みが即座に可能（Redo リカバリ待ち時間なし！）
    let new_primary_conn = cluster.primary_connection();
    new_primary_conn.execute("UPDATE cluster_state SET leader = 'replica-node-promoted' WHERE epoch = 1;").unwrap();

    let rows = new_primary_conn.query("SELECT leader FROM cluster_state WHERE epoch = 1;").unwrap();
    assert_eq!(rows[0].get_as::<String>(0).unwrap(), "replica-node-promoted");

    // 古い Primary からの遅延書き込みは Fencing Token 不正でストレージ層が拒否 (Split-Brain 防止)
    let stale_write_res = old_primary_conn.execute("INSERT INTO cluster_state VALUES (99, 'zombie-primary');");
    assert!(stale_write_res.is_err(), "Stale primary write must be rejected by fencing token");
}

#[test]
fn test_transactional_queue_on_decoupled_cluster() {
    let cluster = DecoupledCluster::new_3nodes("test_mq_decoupled", 1).unwrap();
    let primary_conn = cluster.primary_connection();
    let replica_conn = cluster.replica_connection(0).unwrap();

    // 1. トランザクショナル・キューテーブルを作成
    primary_conn.execute("CREATE QUEUE TABLE event_stream (payload VARCHAR) WITH (RETENTION_HOURS = 24);").unwrap();

    // 2. メッセージをエンキュー
    primary_conn.execute("INSERT INTO event_stream (payload) VALUES ('OrderCreated');").unwrap();
    primary_conn.execute("INSERT INTO event_stream (payload) VALUES ('PaymentReceived');").unwrap();

    // 3. リードレプリカからキューメッセージを参照
    let rows = replica_conn.query("SELECT _offset, payload FROM event_stream ORDER BY _offset;").unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].get_as::<i64>(0).unwrap(), 1);
    assert_eq!(rows[0].get_as::<String>(1).unwrap(), "OrderCreated");
    assert_eq!(rows[1].get_as::<i64>(0).unwrap(), 2);
    assert_eq!(rows[1].get_as::<String>(1).unwrap(), "PaymentReceived");
}
