use std::time::Duration;

use h2::replication::{Instance, InstanceConfig, InstanceRole, SyncReplicationMode};
use h2::{AcknowledgeMode, H2Error, JmsConnectionFactory, Value};

#[test]
fn test_replication_queue_remote_apply() {
    // 1. Primary (Read-Write) 起動
    let p_config = InstanceConfig {
        role: InstanceRole::Primary {
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            sync_mode: SyncReplicationMode::RemoteApply,
            apply_timeout: Duration::from_secs(5),
        },
    };
    let primary = Instance::open_in_memory(p_config).unwrap();
    let p_addr = primary.replication_addr().unwrap();

    // 2. Standby (Read-Only) 起動
    let s_config = InstanceConfig::standby(p_addr);
    let standby = Instance::open_in_memory(s_config).unwrap();

    let p_conn = primary.connect().unwrap();
    let s_conn = standby.connect().unwrap();

    // 3. Primary でキューテーブル作成
    p_conn
        .execute("CREATE QUEUE TABLE sync_events (payload TEXT) WITH (RETENTION_TIME = '1 DAY');")
        .unwrap();

    // 4. Primary で JMS 送信
    let p_factory = JmsConnectionFactory::new(p_conn.clone());
    let p_jms = p_factory.create_connection().unwrap();
    let p_session = p_jms.create_session(false, AcknowledgeMode::AutoAcknowledge).unwrap();
    let queue = p_session.create_queue("sync_events").unwrap();
    let producer = p_session.create_producer(&queue).unwrap();

    producer
        .send(p_session.create_text_message("cluster_event_1").unwrap())
        .unwrap();
    producer
        .send(p_session.create_text_message("cluster_event_2").unwrap())
        .unwrap();
    producer
        .send(p_session.create_text_message("cluster_event_3").unwrap())
        .unwrap();

    // 5. Standby で即座に全メッセージが同一オフセットで可視
    let rows = s_conn.query("SELECT _offset, payload FROM sync_events;").unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].values[0], Value::BigInt(1));
    assert_eq!(rows[0].values[1], Value::String("cluster_event_1".to_string()));
    assert_eq!(rows[1].values[0], Value::BigInt(2));
    assert_eq!(rows[1].values[1], Value::String("cluster_event_2".to_string()));
    assert_eq!(rows[2].values[0], Value::BigInt(3));
    assert_eq!(rows[2].values[1], Value::String("cluster_event_3".to_string()));

    // 6. Standby 側から JMS Consumer で読み取り可能
    let s_factory = JmsConnectionFactory::new(s_conn.clone());
    let s_jms = s_factory.create_connection().unwrap();
    let s_session = s_jms.create_session(false, AcknowledgeMode::AutoAcknowledge).unwrap();
    let s_consumer = s_session.create_consumer(&queue, "standby-reader").unwrap();

    let m1 = s_consumer.receive_no_wait().unwrap().unwrap();
    assert_eq!(m1.get_offset(), 1);
    assert_eq!(m1.get_text(), "cluster_event_1");

    let m2 = s_consumer.receive_no_wait().unwrap().unwrap();
    assert_eq!(m2.get_offset(), 2);
    assert_eq!(m2.get_text(), "cluster_event_2");

    // 7. Standby 側での書き込みは ReadOnly で拒否
    let write_err = s_conn.execute("INSERT INTO sync_events (payload) VALUES ('illegal_write');");
    assert!(matches!(write_err, Err(H2Error::ReadOnly(_))));

    let ddl_err = s_conn.execute("CREATE QUEUE TABLE fail_q (payload TEXT);");
    assert!(matches!(ddl_err, Err(H2Error::ReadOnly(_))));
}
