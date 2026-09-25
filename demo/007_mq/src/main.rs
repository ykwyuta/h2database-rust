use h2::jms::{AcknowledgeMode, JmsConnectionFactory};
use h2::{Connection, H2Result};

fn main() -> H2Result<()> {
    println!("============================================================");
    println!("  H2 Database in Rust - Transactional Queue Table (MQ) Demo");
    println!("  (Native JMS API + Kafka-style Seeking + Dual Retention GC)");
    println!("============================================================");

    // 1. インメモリデータベースの初期化
    let conn = Connection::open_in_memory()?;

    // 2. トランザクショナル・キューテーブルの作成 (DDL)
    // 保持期間 24 時間、最大容量 10MB の設定
    conn.execute(
        "CREATE QUEUE TABLE orders_queue (payload VARCHAR) 
         WITH (RETENTION_HOURS = 24, MAX_BYTES = 10485760);"
    )?;
    println!("[1] Created transactional queue table 'orders_queue'.");

    // 3. SQL からの透過的 INSERT (エンキュー) & SELECT (デキュー)
    println!("\n[2] Producing and consuming via standard SQL:");
    conn.execute("INSERT INTO orders_queue (payload) VALUES ('Order #1001 Created');")?;
    conn.execute("INSERT INTO orders_queue (payload) VALUES ('Order #1002 Created');")?;

    let sql_rows = conn.query("SELECT _offset, _msg_id, payload FROM orders_queue WHERE _offset >= 1 ORDER BY _offset;")?;
    for row in sql_rows {
        let offset: i64 = row.get_as(0)?;
        let msg_id: String = row.get_as(1)?;
        let payload: String = row.get_as(2)?;
        println!("    [SQL Read] Offset: {offset} | MsgID: {msg_id} | Payload: {payload}");
    }

    // 4. JMS 2.0/3.0 準拠 API によるメッセージ送受信
    println!("\n[3] Using JMS 2.0/3.0 Compliant API:");
    let factory = JmsConnectionFactory::new(conn.clone());
    let jms_conn = factory.create_connection()?;
    let session = jms_conn.create_session(false, AcknowledgeMode::AutoAcknowledge)?;
    let queue = session.create_queue("orders_queue")?;

    let producer = session.create_producer(&queue)?;
    let consumer = session.create_consumer(&queue, "payment-processor")?;

    // メッセージの送信
    producer.send(session.create_text_message("Payment Authorized for #1001")?)?;
    producer.send(session.create_text_message("Inventory Reserved for #1001")?)?;
    println!("    JMS Producer sent 2 messages.");

    // メッセージの受信 (非ブロッキング)
    println!("\n[4] Consuming via JMS Consumer:");
    while let Some(msg) = consumer.receive(None)? {
        println!("    [JMS Consumer] Received Offset: {} -> Text: '{}'", msg.get_offset(), msg.get_text());
    }

    // 5. Kafka 風オフセットシーク (seek / rewind)
    println!("\n[5] Kafka-style Seeking & Replaying:");
    println!("    Rewinding consumer offset back to beginning (Offset 1)...");
    consumer.seek_to_beginning()?;

    let replayed_msg1 = consumer.receive(None)?.unwrap();
    println!("    [Replayed 1] Offset: {} -> Text: '{}'", replayed_msg1.get_offset(), replayed_msg1.get_text());

    println!("    Seeking directly to Offset 3...");
    consumer.seek(3)?;
    let jumped_msg = consumer.receive(None)?.unwrap();
    println!("    [Jumped] Offset: {} -> Text: '{}'", jumped_msg.get_offset(), jumped_msg.get_text());

    // 6. トランザクショナル Outbox パターンの完全解消 (同一 ACID トランザクション)
    println!("\n[6] Transactional Outbox Demo (Atomic Table Update + Enqueue):");
    conn.execute("CREATE TABLE inventory (sku VARCHAR PRIMARY KEY, qty INT);")?;
    conn.execute("INSERT INTO inventory VALUES ('ITEM-99', 50);")?;

    // トランザクション開始
    let tx = conn.transaction()?;
    tx.execute("UPDATE inventory SET qty = qty - 1 WHERE sku = 'ITEM-99';")?;
    tx.execute("INSERT INTO orders_queue (payload) VALUES ('Order Placed: ITEM-99 (qty -1)');")?;
    tx.commit()?;
    println!("    Committed transaction! Inventory updated and Queue message emitted atomically.");

    let inv_row = conn.query("SELECT qty FROM inventory WHERE sku = 'ITEM-99';")?;
    println!("    Current inventory qty: {}", inv_row[0].get_as::<i32>(0)?);

    consumer.seek_to_end()?;
    // 最新投入のメッセージを取得
    consumer.seek(5)?;
    if let Some(msg) = consumer.receive(None)? {
        println!("    [Outbox Message] Offset: {} -> Text: '{}'", msg.get_offset(), msg.get_text());
    }

    println!("\n[SUCCESS] Transactional queue table demo completed cleanly!");
    Ok(())
}
