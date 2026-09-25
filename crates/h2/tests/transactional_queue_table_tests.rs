use std::thread::sleep;
use std::time::Duration;

use h2::{
    AcknowledgeMode, Connection, H2Error, JmsConnectionFactory, Value,
};


#[test]
fn test_queue_table_ddl_and_guards() {
    let conn = Connection::open_in_memory().unwrap();

    // 1. キューテーブル作成 (WITH 句付き)
    conn.execute(
        "CREATE QUEUE TABLE order_events (
            payload TEXT,
            topic VARCHAR DEFAULT 'orders'
        ) WITH (
            RETENTION_TIME = '7 DAYS',
            MAX_BYTES = '10GB'
        );",
    )
    .unwrap();

    // 2. ガードテスト: UPDATE 禁止
    let update_res = conn.execute("UPDATE order_events SET payload = 'modified';");
    assert!(update_res.is_err());
    match update_res {
        Err(H2Error::Unsupported(msg)) => {
            assert!(msg.contains("UPDATE is not allowed on Queue Table"));
        }
        other => panic!("Expected H2Error::Unsupported, got {:?}", other),
    }

    // 3. ガードテスト: DELETE 禁止
    let delete_res = conn.execute("DELETE FROM order_events WHERE _offset = 1;");
    assert!(delete_res.is_err());
    match delete_res {
        Err(H2Error::Unsupported(msg)) => {
            assert!(msg.contains("DELETE is not allowed on Queue Table"));
        }
        other => panic!("Expected H2Error::Unsupported, got {:?}", other),
    }

    // 4. ガードテスト: 二次インデックス禁止
    let index_res = conn.execute("CREATE INDEX idx_topic ON order_events(topic);");
    assert!(index_res.is_err());
    match index_res {
        Err(H2Error::Unsupported(msg)) => {
            assert!(msg.contains("Secondary indexes are not allowed on Queue Table"));
        }
        other => panic!("Expected H2Error::Unsupported, got {:?}", other),
    }

    // 5. ガードテスト: TRUNCATE 禁止
    let trunc_res = conn.execute("TRUNCATE TABLE order_events;");
    assert!(trunc_res.is_err());
    match trunc_res {
        Err(H2Error::Unsupported(msg)) => {
            assert!(msg.contains("TRUNCATE is not allowed on Queue Table"));
        }
        other => panic!("Expected H2Error::Unsupported, got {:?}", other),
    }
}

#[test]
fn test_queue_table_insert_and_select_offset_restrictions() {
    let conn = Connection::open_in_memory().unwrap();

    conn.execute("CREATE QUEUE TABLE event_log (payload TEXT);").unwrap();

    // 通常の INSERT (payload のみ指定)
    conn.execute("INSERT INTO event_log (payload) VALUES ('msg_1');").unwrap();
    conn.execute("INSERT INTO event_log (payload) VALUES ('msg_2'), ('msg_3');").unwrap();

    // 全件 SELECT
    let rows = conn.query("SELECT * FROM event_log;").unwrap();
    assert_eq!(rows.len(), 3);

    // システム列の確認: 0: _offset, 1: _timestamp, 2: _msg_id, 3: _correlation_id, 4: payload
    for (i, r) in rows.iter().enumerate() {
        let expected_offset = (i + 1) as i64;
        assert_eq!(r.values[0], Value::BigInt(expected_offset));
        assert!(matches!(r.values[1], Value::Timestamp(_)));
        if let Value::String(ref mid) = r.values[2] {
            assert!(mid.starts_with(&format!("ID:h2-mq-{}", expected_offset)));
        } else {
            panic!("Expected String for _msg_id");
        }
        assert_eq!(r.values[4], Value::String(format!("msg_{}", expected_offset)));
    }

    // WHERE _offset のみ許可テスト
    let offset_rows = conn.query("SELECT _offset, payload FROM event_log WHERE _offset >= 2;").unwrap();
    assert_eq!(offset_rows.len(), 2);
    assert_eq!(offset_rows[0].values[0], Value::BigInt(2));
    assert_eq!(offset_rows[1].values[0], Value::BigInt(3));

    let between_rows = conn.query("SELECT _offset FROM event_log WHERE _offset BETWEEN 1 AND 2;").unwrap();
    assert_eq!(between_rows.len(), 2);

    // WHERE 句に _offset 以外の列を指定した場合はエラー
    let bad_filter = conn.query("SELECT * FROM event_log WHERE payload = 'msg_1';");
    assert!(bad_filter.is_err());
    match bad_filter {
        Err(H2Error::Unsupported(msg)) => {
            assert!(msg.contains("Only '_offset' column is allowed in WHERE clause on Queue Table"));
        }
        other => panic!("Expected H2Error::Unsupported, got {:?}", other),
    }
}

#[test]
fn test_transactional_outbox_integration() {
    let conn = Connection::open_in_memory().unwrap();

    // 業務テーブルとキューテーブル
    conn.execute("CREATE TABLE orders (id INT PRIMARY KEY, item VARCHAR(64), amount INT);").unwrap();
    conn.execute("CREATE QUEUE TABLE order_queue (payload TEXT);").unwrap();

    let factory = JmsConnectionFactory::new(conn.clone());
    let jms_conn = factory.create_connection().unwrap();
    let session = jms_conn.create_session(false, AcknowledgeMode::AutoAcknowledge).unwrap();
    let queue = session.create_queue("order_queue").unwrap();
    let producer = session.create_producer(&queue).unwrap();

    // 1. 同一トランザクション内での業務テーブル INSERT とエンキューのコミット
    {
        let tx = conn.transaction().unwrap();
        tx.execute("INSERT INTO orders VALUES (101, 'MacBook', 300000);").unwrap();

        let mut msg = session.create_text_message("{\"order_id\": 101, \"status\": \"CREATED\"}").unwrap();
        msg.set_jms_correlation_id(Some("REQ-001".to_string()));
        producer.send_with_tx(&tx, msg).unwrap();

        tx.commit().unwrap();
    }

    // 確認: 業務テーブル・キュー両方に反映されている
    let order_rows = conn.query("SELECT * FROM orders WHERE id = 101;").unwrap();
    assert_eq!(order_rows.len(), 1);
    let queue_rows = conn.query("SELECT * FROM order_queue;").unwrap();
    assert_eq!(queue_rows.len(), 1);

    // 2. トランザクションのロールバック（業務テーブルもキューも一切残らない）
    {
        let tx = conn.transaction().unwrap();
        tx.execute("INSERT INTO orders VALUES (102, 'iPad', 120000);").unwrap();

        let msg = session.create_text_message("{\"order_id\": 102, \"status\": \"CREATED\"}").unwrap();
        producer.send_with_tx(&tx, msg).unwrap();

        tx.rollback().unwrap();
    }

    // 確認: 102 は両方に残っていない
    let order_rows2 = conn.query("SELECT * FROM orders WHERE id = 102;").unwrap();
    assert_eq!(order_rows2.len(), 0);
    let queue_rows2 = conn.query("SELECT * FROM order_queue;").unwrap();
    assert_eq!(queue_rows2.len(), 1); // 101 のみ
}

#[test]
fn test_jms_api_and_kafka_seeking() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute("CREATE QUEUE TABLE payment_events (payload TEXT);").unwrap();

    let factory = JmsConnectionFactory::new(conn.clone());
    let jms_conn = factory.create_connection().unwrap();
    let session = jms_conn.create_session(false, AcknowledgeMode::AutoAcknowledge).unwrap();
    let queue = session.create_queue("payment_events").unwrap();

    let producer = session.create_producer(&queue).unwrap();
    let consumer = session.create_consumer(&queue, "payment-processor").unwrap();

    // 5件送信
    for i in 1..=5 {
        let mut msg = session.create_text_message(format!("pay_{}", i)).unwrap();
        msg.set_string_property("event_type", "PAYMENT_COMPLETED");
        msg.set_int_property("amount", (i * 1000) as i64);
        producer.send(msg).unwrap();
    }

    // 順次受信
    let msg1 = consumer.receive(None).unwrap().unwrap();
    assert_eq!(msg1.get_offset(), 1);
    assert_eq!(msg1.get_text(), "pay_1");

    let msg2 = consumer.receive(None).unwrap().unwrap();
    assert_eq!(msg2.get_offset(), 2);
    assert_eq!(msg2.get_text(), "pay_2");

    // Kafka 風シーク: 先頭に巻き戻し (Rewind to beginning)
    consumer.seek_to_beginning().unwrap();
    let rewind_msg = consumer.receive(None).unwrap().unwrap();
    assert_eq!(rewind_msg.get_offset(), 1);
    assert_eq!(rewind_msg.get_text(), "pay_1");

    // 特定オフセットにシーク (Offset 4)
    consumer.seek(4).unwrap();
    let msg4 = consumer.receive(None).unwrap().unwrap();
    assert_eq!(msg4.get_offset(), 4);
    assert_eq!(msg4.get_text(), "pay_4");

    // 末尾にシーク
    consumer.seek_to_end().unwrap();
    let none_msg = consumer.receive(None).unwrap();
    assert!(none_msg.is_none());

    // 新規メッセージを追加投入
    producer.send(session.create_text_message("pay_6").unwrap()).unwrap();

    // 末尾で待っていた consumer が pay_6 を受信
    let msg6 = consumer.receive(None).unwrap().unwrap();
    assert_eq!(msg6.get_offset(), 6);
    assert_eq!(msg6.get_text(), "pay_6");
}

#[test]
fn test_retention_time_and_head_truncation_gc() {
    let conn = Connection::open_in_memory().unwrap();

    // 保持期間 150 ミリ秒のキューテーブル
    conn.execute(
        "CREATE QUEUE TABLE temp_events (payload TEXT) WITH (RETENTION_TIME = '150 MS');",
    )
    .unwrap();

    let factory = JmsConnectionFactory::new(conn.clone());
    let jms_conn = factory.create_connection().unwrap();
    let session = jms_conn.create_session(false, AcknowledgeMode::AutoAcknowledge).unwrap();
    let queue = session.create_queue("temp_events").unwrap();

    let producer = session.create_producer(&queue).unwrap();
    let consumer = session.create_consumer(&queue, "worker-1").unwrap();

    // 2件投入
    producer.send(session.create_text_message("old_msg_1").unwrap()).unwrap();
    producer.send(session.create_text_message("old_msg_2").unwrap()).unwrap();

    assert_eq!(conn.query("SELECT * FROM temp_events;").unwrap().len(), 2);

    // 保持期間を経過させる
    sleep(Duration::from_millis(200));

    // 新しいメッセージを投入（この INSERT 時に古いメッセージが自動パージされる）
    producer.send(session.create_text_message("new_msg_3").unwrap()).unwrap();

    // 確認: 古い2件がパージされ、new_msg_3（offset 3）のみ残っている
    let remaining_rows = conn.query("SELECT * FROM temp_events;").unwrap();
    assert_eq!(remaining_rows.len(), 1);
    assert_eq!(remaining_rows[0].values[0], Value::BigInt(3));
    assert_eq!(remaining_rows[0].values[4], Value::String("new_msg_3".to_string()));

    // パージされた古いオフセット（例: 1）を consumer がシークして読み出そうとすると
    // OffsetOutOfRange エラーが返ることを確認
    consumer.seek(1).unwrap();
    let err_res = consumer.receive(None);
    assert!(err_res.is_err());
    match err_res {
        Err(H2Error::OffsetOutOfRange(msg)) => {
            assert!(msg.contains("is out of range"));
            assert!(msg.contains("Oldest available offset"));
        }
        other => panic!("Expected OffsetOutOfRange, got {:?}", other),
    }

    // 有効なオフセット（3）にシークすれば正常に受信可能
    consumer.seek(3).unwrap();
    let valid_msg = consumer.receive(None).unwrap().unwrap();
    assert_eq!(valid_msg.get_offset(), 3);
    assert_eq!(valid_msg.get_text(), "new_msg_3");
}

#[test]
fn test_max_bytes_capacity_head_truncation() {
    let conn = Connection::open_in_memory().unwrap();

    // 容量上限 350 バイトのキューテーブル
    // 1行あたりキー(8B) + ペイロード等で約80〜100バイト
    conn.execute(
        "CREATE QUEUE TABLE size_bounded_queue (payload TEXT) WITH (MAX_BYTES = '300 B');",
    )
    .unwrap();

    let factory = JmsConnectionFactory::new(conn.clone());
    let jms_conn = factory.create_connection().unwrap();
    let session = jms_conn.create_session(false, AcknowledgeMode::AutoAcknowledge).unwrap();
    let queue = session.create_queue("size_bounded_queue").unwrap();
    let producer = session.create_producer(&queue).unwrap();

    // 多数投入して容量上限をわざと大幅に超過させる
    for i in 1..=10 {
        producer.send(session.create_text_message(format!("bounded_message_payload_{}", i)).unwrap()).unwrap();
    }

    // 容量制限により最古メッセージが Head Truncate され、総件数が 10件より少なくなっている
    let rows = conn.query("SELECT * FROM size_bounded_queue;").unwrap();
    assert!(rows.len() < 10);
    assert!(rows.len() >= 1);

    // 残っている最古のオフセットは 1 ではなく途中の番号に進んでいる
    let min_offset_row = conn.query("SELECT MIN(_offset) FROM size_bounded_queue;").unwrap();
    if let Value::BigInt(min_off) = min_offset_row[0].values[0] {
        assert!(min_off > 1, "Oldest messages must have been head truncated");
    }
}

#[test]
fn test_blocking_receive_and_batch() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute("CREATE QUEUE TABLE async_events (payload TEXT);").unwrap();

    let factory = JmsConnectionFactory::new(conn.clone());
    let jms_conn = factory.create_connection().unwrap();
    let session = jms_conn.create_session(false, AcknowledgeMode::AutoAcknowledge).unwrap();
    let queue = session.create_queue("async_events").unwrap();

    let consumer = session.create_consumer(&queue, "worker").unwrap();

    // 1. 別スレッドから遅れてメッセージが送信されるのを待機受信 (Blocking receive)
    let conn_clone = conn.clone();
    std::thread::spawn(move || {
        sleep(Duration::from_millis(60));
        let f = JmsConnectionFactory::new(conn_clone);
        let c = f.create_connection().unwrap();
        let s = c.create_session(false, AcknowledgeMode::AutoAcknowledge).unwrap();
        let q = s.create_queue("async_events").unwrap();
        let p = s.create_producer(&q).unwrap();
        p.send(s.create_text_message("delayed_event_1").unwrap()).unwrap();
    });

    // タイムアウト 500ms で待機
    let start = std::time::Instant::now();
    let msg = consumer.receive(Some(Duration::from_millis(600))).unwrap();
    assert!(msg.is_some());
    assert_eq!(msg.unwrap().get_text(), "delayed_event_1");
    assert!(start.elapsed() >= Duration::from_millis(40));

    // 2. バッチ受信テスト (receive_batch)
    let producer = session.create_producer(&queue).unwrap();
    for i in 2..=5 {
        producer.send(session.create_text_message(format!("batch_event_{}", i)).unwrap()).unwrap();
    }

    let batch = consumer.receive_batch(3, Some(Duration::from_millis(100))).unwrap();
    assert_eq!(batch.len(), 3);
    assert_eq!(batch[0].get_text(), "batch_event_2");
    assert_eq!(batch[1].get_text(), "batch_event_3");
    assert_eq!(batch[2].get_text(), "batch_event_4");

    // 残り 1 件
    let last = consumer.receive_no_wait().unwrap();
    assert!(last.is_some());
    assert_eq!(last.unwrap().get_text(), "batch_event_5");
}

#[test]
fn test_rewind_and_bytes_message() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute("CREATE QUEUE TABLE stream_events (payload TEXT);").unwrap();

    let factory = JmsConnectionFactory::new(conn);
    let jms_conn = factory.create_connection().unwrap();
    let session = jms_conn.create_session(false, AcknowledgeMode::AutoAcknowledge).unwrap();
    let queue = session.create_queue("stream_events").unwrap();

    let producer = session.create_producer(&queue).unwrap();
    let consumer = session.create_consumer(&queue, "stream-app").unwrap();

    // バイナリメッセージ送信
    let mut bytes_msg = session.create_bytes_message(vec![0xDE, 0xAD, 0xBE, 0xEF]).unwrap();
    bytes_msg.set_jms_correlation_id(Some("CORR-BYTES-1".to_string()));
    producer.send_bytes(bytes_msg).unwrap();

    // テキストメッセージ送信
    producer.send(session.create_text_message("text_stream_2").unwrap()).unwrap();
    producer.send(session.create_text_message("text_stream_3").unwrap()).unwrap();

    // 3件消費
    let m1 = consumer.receive_no_wait().unwrap().unwrap();
    assert_eq!(m1.get_offset(), 1);
    assert_eq!(m1.get_jms_correlation_id(), Some("CORR-BYTES-1"));

    let m2 = consumer.receive_no_wait().unwrap().unwrap();
    assert_eq!(m2.get_offset(), 2);

    let m3 = consumer.receive_no_wait().unwrap().unwrap();
    assert_eq!(m3.get_offset(), 3);

    // Kafka 風 rewind: 2件巻き戻し (Offset 2 に戻る)
    consumer.rewind(2).unwrap();
    assert_eq!(consumer.get_current_offset(), 2);
    let re_m2 = consumer.receive_no_wait().unwrap().unwrap();
    assert_eq!(re_m2.get_offset(), 2);
    assert_eq!(re_m2.get_text(), "text_stream_2");
}

#[test]
fn test_multiple_consumer_groups_independent_offsets() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute("CREATE QUEUE TABLE shared_queue (payload TEXT);").unwrap();

    let factory = JmsConnectionFactory::new(conn);
    let jms_conn = factory.create_connection().unwrap();
    let session = jms_conn.create_session(false, AcknowledgeMode::AutoAcknowledge).unwrap();
    let queue = session.create_queue("shared_queue").unwrap();

    let producer = session.create_producer(&queue).unwrap();
    for i in 1..=4 {
        producer.send(session.create_text_message(format!("shared_msg_{}", i)).unwrap()).unwrap();
    }

    // 2 つの異なるコンシューマグループを作成
    let consumer_a = session.create_consumer(&queue, "analytics-group").unwrap();
    let consumer_b = session.create_consumer(&queue, "audit-group").unwrap();

    // consumer_a は 3 件消費
    let a1 = consumer_a.receive_no_wait().unwrap().unwrap();
    let a2 = consumer_a.receive_no_wait().unwrap().unwrap();
    let a3 = consumer_a.receive_no_wait().unwrap().unwrap();
    assert_eq!(a1.get_offset(), 1);
    assert_eq!(a2.get_offset(), 2);
    assert_eq!(a3.get_offset(), 3);

    // consumer_b は 1 件のみ消費
    let b1 = consumer_b.receive_no_wait().unwrap().unwrap();
    assert_eq!(b1.get_offset(), 1);

    // 再度新セッションで立ち上げても、それぞれのグループのオフセットから再開される
    let consumer_a_resume = session.create_consumer(&queue, "analytics-group").unwrap();
    let a_next = consumer_a_resume.receive_no_wait().unwrap().unwrap();
    assert_eq!(a_next.get_offset(), 4);

    let consumer_b_resume = session.create_consumer(&queue, "audit-group").unwrap();
    let b_next = consumer_b_resume.receive_no_wait().unwrap().unwrap();
    assert_eq!(b_next.get_offset(), 2);
}

#[test]
fn test_background_retention_cleaner() {
    use h2::QueueRetentionCleaner;

    let conn = Connection::open_in_memory().unwrap();
    conn.execute(
        "CREATE QUEUE TABLE bg_events (payload TEXT) WITH (RETENTION_TIME = '100 MS');",
    )
    .unwrap();

    let factory = JmsConnectionFactory::new(conn.clone());
    let jms_conn = factory.create_connection().unwrap();
    let session = jms_conn.create_session(false, AcknowledgeMode::AutoAcknowledge).unwrap();
    let queue = session.create_queue("bg_events").unwrap();
    let producer = session.create_producer(&queue).unwrap();

    // 3件投入
    producer.send(session.create_text_message("bg_1").unwrap()).unwrap();
    producer.send(session.create_text_message("bg_2").unwrap()).unwrap();
    producer.send(session.create_text_message("bg_3").unwrap()).unwrap();

    assert_eq!(conn.query("SELECT * FROM bg_events;").unwrap().len(), 3);

    // 50ms 周期のバックグラウンドクリーナーを起動
    let mut cleaner = QueueRetentionCleaner::start(conn.clone(), Duration::from_millis(50));

    // 250ms 待機（この間 INSERT は一切行われないが、バックグラウンドスレッドが自律的にパージする）
    sleep(Duration::from_millis(300));

    // パージされて 0 件になっていることを確認
    let remaining = conn.query("SELECT * FROM bg_events;").unwrap();
    assert_eq!(remaining.len(), 0);

    cleaner.stop();
}

#[test]
fn test_file_persistence_and_lifecycle() {
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("queue_persist_test.db");

    // 1. ファイルベースで作成・エンキュー
    {
        let conn = Connection::open(&db_path).unwrap();
        conn.execute(
            "CREATE QUEUE TABLE IF NOT EXISTS durable_queue (
                payload TEXT,
                tag VARCHAR DEFAULT 'default'
            ) WITH (
                RETENTION_TIME = '30 DAYS',
                MAX_BYTES = '1GB'
            );",
        )
        .unwrap();

        let factory = JmsConnectionFactory::new(conn.clone());
        let jms_conn = factory.create_connection().unwrap();
        let session = jms_conn.create_session(false, AcknowledgeMode::AutoAcknowledge).unwrap();
        let queue = session.create_queue("durable_queue").unwrap();
        let producer = session.create_producer(&queue).unwrap();

        producer.send(session.create_text_message("durable_1").unwrap()).unwrap();
        producer.send(session.create_text_message("durable_2").unwrap()).unwrap();

        let consumer = session.create_consumer(&queue, "durable-group").unwrap();
        let m1 = consumer.receive_no_wait().unwrap().unwrap();
        assert_eq!(m1.get_offset(), 1);
        assert_eq!(m1.get_text(), "durable_1");
        consumer.commit_offset(2).unwrap();
    }

    // 2. DB を再オープンして永続化されたキューから再開
    {
        let conn = Connection::open(&db_path).unwrap();

        // SELECT の確認
        let rows = conn.query("SELECT _offset, payload FROM durable_queue WHERE _offset = 2;").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].values[0], Value::BigInt(2));
        assert_eq!(rows[0].values[1], Value::String("durable_2".to_string()));

        // キューガードが再オープン後も有効か
        let update_res = conn.execute("UPDATE durable_queue SET payload = 'fail';");
        assert!(matches!(update_res, Err(H2Error::Unsupported(_))));

        // コミット済みオフセットから再開 (Offset 2 を受信)
        let factory = JmsConnectionFactory::new(conn.clone());
        let jms_conn = factory.create_connection().unwrap();
        let session = jms_conn.create_session(false, AcknowledgeMode::AutoAcknowledge).unwrap();
        let queue = session.create_queue("durable_queue").unwrap();
        let consumer = session.create_consumer(&queue, "durable-group").unwrap();

        let m2 = consumer.receive_no_wait().unwrap().unwrap();
        assert_eq!(m2.get_offset(), 2);
        assert_eq!(m2.get_text(), "durable_2");

        // DROP TABLE のライフサイクル
        conn.execute("DROP TABLE durable_queue;").unwrap();
        let select_res = conn.query("SELECT * FROM durable_queue;");
        assert!(select_res.is_err());
    }
}


