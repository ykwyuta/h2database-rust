//! JMS (Java Message Service) 準拠 API & Kafka 風オフセットシーク機能
//!
//! リレーショナルデータベースのローカル MVCC トランザクション（ACID）と完全に統合された
//! トランザクショナル・メッセージキュー操作を提供します。

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;

use crate::{Connection, Transaction};
use h2_types::{H2Error, H2Result, Value};

/// JMS 確認応答モード
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcknowledgeMode {
    /// メッセージ受信時に自動的にオフセットを確定
    AutoAcknowledge,
    /// クライアントが明示的に commit_offset を呼び出すまで確定しない
    ClientAcknowledge,
    /// 遅延確認（重複の可能性を許容）
    DupsOkAcknowledge,
    /// セッション/DBトランザクションと連動してコミット/ロールバック
    SessionTransacted,
}

/// JMS コネクションファクトリ
#[derive(Clone)]
pub struct JmsConnectionFactory {
    conn: Connection,
}

impl JmsConnectionFactory {
    pub fn new(conn: Connection) -> Self {
        Self { conn }
    }

    pub fn open<P: AsRef<Path>>(path: P) -> H2Result<Self> {
        let conn = Connection::open(path)?;
        Ok(Self { conn })
    }

    pub fn open_in_memory() -> H2Result<Self> {
        let conn = Connection::open_in_memory()?;
        Ok(Self { conn })
    }

    pub fn create_connection(&self) -> H2Result<JmsConnection> {
        Ok(JmsConnection {
            conn: self.conn.new_session(),
        })
    }
}

/// JMS コネクション
pub struct JmsConnection {
    conn: Connection,
}

impl JmsConnection {
    pub fn create_session(&self, transacted: bool, ack_mode: AcknowledgeMode) -> H2Result<JmsSession> {
        let session_conn = self.conn.new_session();
        Ok(JmsSession {
            conn: session_conn,
            transacted,
            ack_mode,
            active_tx: Arc::new(Mutex::new(None)),
        })
    }

    pub fn close(&self) -> H2Result<()> {
        Ok(())
    }

    pub fn connection(&self) -> &Connection {
        &self.conn
    }
}

/// JMS キュー宛先
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JmsQueue {
    name: String,
}

impl JmsQueue {
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into() }
    }

    pub fn get_queue_name(&self) -> &str {
        &self.name
    }
}

/// JMS テキストメッセージ
#[derive(Debug, Clone)]
pub struct JmsTextMessage {
    pub(crate) offset: u64,
    pub(crate) timestamp_ms: u64,
    pub(crate) jms_message_id: String,
    pub(crate) jms_correlation_id: Option<String>,
    pub(crate) jms_reply_to: Option<String>,
    pub(crate) jms_type: Option<String>,
    pub(crate) destination: String,
    pub(crate) priority: u8,
    pub(crate) delivery_mode: u8,
    pub(crate) expiration: u64,
    pub(crate) redelivered: bool,
    pub(crate) properties: HashMap<String, Value>,
    pub(crate) text: String,
}

impl JmsTextMessage {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            offset: 0,
            timestamp_ms: 0,
            jms_message_id: String::new(),
            jms_correlation_id: None,
            jms_reply_to: None,
            jms_type: None,
            destination: String::new(),
            priority: 4,
            delivery_mode: 2, // PERSISTENT
            expiration: 0,
            redelivered: false,
            properties: HashMap::new(),
            text: text.into(),
        }
    }

    pub fn get_text(&self) -> &str {
        &self.text
    }

    pub fn set_text(&mut self, text: impl Into<String>) {
        self.text = text.into();
    }

    pub fn get_offset(&self) -> u64 {
        self.offset
    }

    pub fn get_timestamp(&self) -> u64 {
        self.timestamp_ms
    }

    pub fn get_jms_message_id(&self) -> &str {
        &self.jms_message_id
    }

    pub fn set_jms_message_id(&mut self, id: impl Into<String>) {
        self.jms_message_id = id.into();
    }

    pub fn get_jms_correlation_id(&self) -> Option<&str> {
        self.jms_correlation_id.as_deref()
    }

    pub fn set_jms_correlation_id(&mut self, id: Option<String>) {
        self.jms_correlation_id = id;
    }

    pub fn get_jms_reply_to(&self) -> Option<&str> {
        self.jms_reply_to.as_deref()
    }

    pub fn set_jms_reply_to(&mut self, reply_to: Option<String>) {
        self.jms_reply_to = reply_to;
    }

    pub fn get_jms_type(&self) -> Option<&str> {
        self.jms_type.as_deref()
    }

    pub fn set_jms_type(&mut self, jms_type: Option<String>) {
        self.jms_type = jms_type;
    }

    pub fn get_jms_priority(&self) -> u8 {
        self.priority
    }

    pub fn set_jms_priority(&mut self, priority: u8) {
        self.priority = priority;
    }

    pub fn get_jms_delivery_mode(&self) -> u8 {
        self.delivery_mode
    }

    pub fn set_jms_delivery_mode(&mut self, delivery_mode: u8) {
        self.delivery_mode = delivery_mode;
    }

    pub fn get_jms_expiration(&self) -> u64 {
        self.expiration
    }

    pub fn set_jms_expiration(&mut self, expiration: u64) {
        self.expiration = expiration;
    }

    pub fn get_jms_redelivered(&self) -> bool {
        self.redelivered
    }

    pub fn set_jms_redelivered(&mut self, redelivered: bool) {
        self.redelivered = redelivered;
    }

    pub fn get_destination(&self) -> &str {
        &self.destination
    }

    pub fn set_destination(&mut self, destination: impl Into<String>) {
        self.destination = destination.into();
    }

    pub fn set_string_property(&mut self, name: impl Into<String>, value: impl Into<String>) {
        self.properties.insert(name.into(), Value::String(value.into()));
    }

    pub fn get_string_property(&self, name: &str) -> Option<String> {
        match self.properties.get(name) {
            Some(Value::String(s)) => Some(s.clone()),
            _ => None,
        }
    }

    pub fn set_int_property(&mut self, name: impl Into<String>, value: i64) {
        self.properties.insert(name.into(), Value::BigInt(value));
    }

    pub fn get_int_property(&self, name: &str) -> Option<i64> {
        match self.properties.get(name) {
            Some(Value::BigInt(v)) => Some(*v),
            Some(Value::Integer(v)) => Some(*v as i64),
            _ => None,
        }
    }

    pub fn set_double_property(&mut self, name: impl Into<String>, value: f64) {
        self.properties.insert(name.into(), Value::Double(value));
    }

    pub fn get_double_property(&self, name: &str) -> Option<f64> {
        match self.properties.get(name) {
            Some(Value::Double(v)) => Some(*v),
            Some(Value::Float(v)) => Some(*v as f64),
            _ => None,
        }
    }

    pub fn set_boolean_property(&mut self, name: impl Into<String>, value: bool) {
        self.properties.insert(name.into(), Value::Boolean(value));
    }

    pub fn get_boolean_property(&self, name: &str) -> Option<bool> {
        match self.properties.get(name) {
            Some(Value::Boolean(b)) => Some(*b),
            _ => None,
        }
    }

    pub fn properties(&self) -> &HashMap<String, Value> {
        &self.properties
    }
}

/// JMS バイナリメッセージ
#[derive(Debug, Clone)]
pub struct JmsBytesMessage {
    pub(crate) offset: u64,
    pub(crate) timestamp_ms: u64,
    pub(crate) jms_message_id: String,
    pub(crate) jms_correlation_id: Option<String>,
    pub(crate) destination: String,
    pub(crate) priority: u8,
    pub(crate) properties: HashMap<String, Value>,
    pub(crate) bytes: Vec<u8>,
}

impl JmsBytesMessage {
    pub fn new(bytes: impl Into<Vec<u8>>) -> Self {
        Self {
            offset: 0,
            timestamp_ms: 0,
            jms_message_id: String::new(),
            jms_correlation_id: None,
            destination: String::new(),
            priority: 4,
            properties: HashMap::new(),
            bytes: bytes.into(),
        }
    }

    pub fn get_bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn set_bytes(&mut self, bytes: impl Into<Vec<u8>>) {
        self.bytes = bytes.into();
    }

    pub fn get_offset(&self) -> u64 {
        self.offset
    }

    pub fn get_timestamp(&self) -> u64 {
        self.timestamp_ms
    }

    pub fn get_jms_message_id(&self) -> &str {
        &self.jms_message_id
    }

    pub fn get_jms_correlation_id(&self) -> Option<&str> {
        self.jms_correlation_id.as_deref()
    }

    pub fn set_jms_correlation_id(&mut self, id: Option<String>) {
        self.jms_correlation_id = id;
    }

    pub fn get_destination(&self) -> &str {
        &self.destination
    }

    pub fn set_destination(&mut self, destination: impl Into<String>) {
        self.destination = destination.into();
    }

    pub fn get_priority(&self) -> u8 {
        self.priority
    }

    pub fn set_priority(&mut self, priority: u8) {
        self.priority = priority;
    }

    pub fn properties(&self) -> &HashMap<String, Value> {

        &self.properties
    }
}

/// JMS セッション
pub struct JmsSession {
    conn: Connection,
    transacted: bool,
    ack_mode: AcknowledgeMode,
    active_tx: Arc<Mutex<Option<Transaction>>>,
}

impl JmsSession {
    pub fn create_queue(&self, queue_name: &str) -> H2Result<JmsQueue> {
        Ok(JmsQueue::new(queue_name))
    }

    pub fn create_producer(&self, queue: &JmsQueue) -> H2Result<JmsMessageProducer> {
        Ok(JmsMessageProducer {
            queue: queue.clone(),
            session_conn: self.conn.clone(),
            is_transacted: self.transacted,
            active_tx: Arc::clone(&self.active_tx),
        })
    }

    pub fn create_consumer(&self, queue: &JmsQueue, group_id: &str) -> H2Result<JmsMessageConsumer> {
        let consumer = JmsMessageConsumer {
            queue: queue.clone(),
            session_conn: self.conn.clone(),
            group_id: group_id.to_string(),
            current_offset: Arc::new(AtomicU64::new(1)),
            is_transacted: self.transacted,
            active_tx: Arc::clone(&self.active_tx),
            ack_mode: self.ack_mode,
        };

        // コミット済みオフセットがあればそこから再開、なければ先頭から
        if let Ok(Some(committed)) = consumer.load_committed_offset() {
            consumer.current_offset.store(committed, Ordering::SeqCst);
        } else {
            let _ = consumer.seek_to_beginning();
        }

        Ok(consumer)
    }

    pub fn create_text_message(&self, text: impl Into<String>) -> H2Result<JmsTextMessage> {
        Ok(JmsTextMessage::new(text))
    }

    pub fn create_bytes_message(&self, bytes: impl Into<Vec<u8>>) -> H2Result<JmsBytesMessage> {
        Ok(JmsBytesMessage::new(bytes))
    }

    pub fn commit(&self) -> H2Result<()> {
        let mut guard = self.active_tx.lock();
        if let Some(tx) = guard.take() {
            tx.commit()?;
        }
        Ok(())
    }

    pub fn rollback(&self) -> H2Result<()> {
        let mut guard = self.active_tx.lock();
        if let Some(tx) = guard.take() {
            tx.rollback()?;
        }
        Ok(())
    }

    pub fn is_transacted(&self) -> bool {
        self.transacted
    }

    pub fn get_acknowledge_mode(&self) -> AcknowledgeMode {
        self.ack_mode
    }

    pub fn connection(&self) -> &Connection {
        &self.conn
    }
}

/// JMS メッセージプロデューサ
pub struct JmsMessageProducer {
    queue: JmsQueue,
    session_conn: Connection,
    is_transacted: bool,
    active_tx: Arc<Mutex<Option<Transaction>>>,
}

impl JmsMessageProducer {
    pub fn get_queue(&self) -> &JmsQueue {
        &self.queue
    }

    /// セッションのトランザクション状態に応じてメッセージを送信
    pub fn send(&self, message: JmsTextMessage) -> H2Result<()> {
        if self.is_transacted {
            let mut guard = self.active_tx.lock();
            if guard.is_none() {
                *guard = Some(self.session_conn.transaction()?);
            }
            let tx = guard.as_ref().unwrap();
            self.send_with_tx(tx, message)
        } else {
            let tx = self.session_conn.transaction()?;
            self.send_with_tx(&tx, message)?;
            tx.commit()?;
            Ok(())
        }
    }

    /// 指定された外部トランザクション内でメッセージを送信（業務テーブルとアトミック連動）
    pub fn send_with_tx(&self, tx: &Transaction, mut message: JmsTextMessage) -> H2Result<()> {
        let q_name = self.queue.get_queue_name();
        message.set_destination(q_name);

        let table_def = tx.engine().catalog().get_table(q_name).ok_or_else(|| {
            H2Error::Catalog(format!("Queue table '{}' not found", q_name))
        })?;

        if !table_def.is_queue {
            return Err(H2Error::Execution(format!("Table '{}' is not a Queue Table", q_name)));
        }

        let has_payload = table_def.columns.iter().any(|c| c.name.eq_ignore_ascii_case("payload"));
        let has_corr = table_def.columns.iter().any(|c| c.name.eq_ignore_ascii_case("_correlation_id"));
        let has_props = table_def.columns.iter().any(|c| c.name.eq_ignore_ascii_case("properties"));

        if has_payload {
            let mut cols = vec!["payload".to_string()];
            let mut vals = vec![Value::String(message.get_text().to_string())];

            if has_corr {
                if let Some(corr_id) = message.get_jms_correlation_id() {
                    cols.push("_correlation_id".to_string());
                    vals.push(Value::String(corr_id.to_string()));
                }
            }

            if has_props && !message.properties.is_empty() {
                cols.push("properties".to_string());
                let json_v = serde_json::to_value(&message.properties).unwrap_or(serde_json::Value::Null);
                vals.push(Value::Json(json_v));
            }

            let col_names = cols.join(", ");
            let placeholders = (1..=cols.len()).map(|i| format!("${}", i)).collect::<Vec<_>>().join(", ");
            let sql = format!("INSERT INTO {} ({}) VALUES ({});", q_name, col_names, placeholders);
            tx.execute_params(&sql, &vals)?;
        } else {
            let sql = format!("INSERT INTO {} VALUES ($1);", q_name);
            tx.execute_params(&sql, &[Value::String(message.get_text().to_string())])?;
        }

        Ok(())
    }

    /// バイナリメッセージを送信
    pub fn send_bytes(&self, message: JmsBytesMessage) -> H2Result<()> {
        let mut txt_msg = JmsTextMessage::new(String::from_utf8_lossy(message.get_bytes()).to_string());
        txt_msg.set_destination(self.queue.get_queue_name());
        txt_msg.set_jms_correlation_id(message.get_jms_correlation_id().map(|s| s.to_string()));
        txt_msg.properties = message.properties;
        self.send(txt_msg)
    }

    /// 指定外部トランザクション内でバイナリメッセージを送信
    pub fn send_bytes_with_tx(&self, tx: &Transaction, message: JmsBytesMessage) -> H2Result<()> {
        let mut txt_msg = JmsTextMessage::new(String::from_utf8_lossy(message.get_bytes()).to_string());
        txt_msg.set_destination(self.queue.get_queue_name());
        txt_msg.set_jms_correlation_id(message.get_jms_correlation_id().map(|s| s.to_string()));
        txt_msg.properties = message.properties;
        self.send_with_tx(tx, txt_msg)
    }
}

/// JMS メッセージコンシューマ（Kafka 風オフセットシーク対応）
pub struct JmsMessageConsumer {
    queue: JmsQueue,
    session_conn: Connection,
    group_id: String,
    current_offset: Arc<AtomicU64>,
    is_transacted: bool,
    active_tx: Arc<Mutex<Option<Transaction>>>,
    ack_mode: AcknowledgeMode,
}

impl JmsMessageConsumer {
    pub fn get_queue(&self) -> &JmsQueue {
        &self.queue
    }

    pub fn get_group_id(&self) -> &str {
        &self.group_id
    }

    pub fn get_current_offset(&self) -> u64 {
        self.current_offset.load(Ordering::SeqCst)
    }

    /// 読み取りオフセットを任意の位置にシーク
    pub fn seek(&self, offset: u64) -> H2Result<()> {
        self.current_offset.store(offset, Ordering::SeqCst);
        Ok(())
    }

    /// 現在保持されている最古のメッセージ位置に巻き戻し (Rewind to beginning)
    pub fn seek_to_beginning(&self) -> H2Result<()> {
        let q_name = self.queue.get_queue_name();
        let rows = self.session_conn.query(&format!("SELECT MIN(_offset) FROM {};", q_name))?;
        if let Some(row) = rows.first() {
            if let Some(Value::BigInt(min_off)) = row.values.first() {
                self.current_offset.store(*min_off as u64, Ordering::SeqCst);
                return Ok(());
            }
        }
        self.current_offset.store(1, Ordering::SeqCst);
        Ok(())
    }

    /// 最新末尾（次に届く新規メッセージの位置）へシーク
    pub fn seek_to_end(&self) -> H2Result<()> {
        let q_name = self.queue.get_queue_name();
        let rows = self.session_conn.query(&format!("SELECT MAX(_offset) FROM {};", q_name))?;
        if let Some(row) = rows.first() {
            if let Some(Value::BigInt(max_off)) = row.values.first() {
                self.current_offset.store((*max_off as u64) + 1, Ordering::SeqCst);
                return Ok(());
            }
        }
        self.current_offset.store(1, Ordering::SeqCst);
        Ok(())
    }

    /// 指定した件数だけ過去へ巻き戻す
    pub fn rewind(&self, count: u64) -> H2Result<()> {
        let current = self.current_offset.load(Ordering::SeqCst);
        let target = current.saturating_sub(count).max(1);
        self.seek(target)
    }

    /// 指定したコミット日時（UNIXエポックミリ秒）以降の最初のメッセージへシーク
    pub fn seek_to_timestamp(&self, timestamp_ms: u64) -> H2Result<()> {
        let q_name = self.queue.get_queue_name();
        let rows = self.session_conn.query(&format!(
            "SELECT _offset, _timestamp FROM {} ORDER BY _offset ASC;",
            q_name
        ))?;

        for r in rows {
            if let (Some(Value::BigInt(off)), Some(ts_val)) = (r.values.get(0), r.values.get(1)) {
                let ms = match ts_val {
                    Value::Timestamp(dt) => dt.timestamp_millis() as u64,
                    _ => 0,
                };
                if ms >= timestamp_ms {
                    self.current_offset.store(*off as u64, Ordering::SeqCst);
                    return Ok(());
                }
            }
        }

        self.seek_to_end()
    }

    /// コンシューマグループのオフセットをストアに保存
    pub fn commit_offset(&self, offset: u64) -> H2Result<()> {
        let tx = self.session_conn.transaction()?;
        self.commit_offset_with_tx(&tx, offset)?;
        tx.commit()?;
        Ok(())
    }

    /// 指定トランザクション内でコンシューマグループのオフセットを保存
    pub fn commit_offset_with_tx(&self, tx: &Transaction, offset: u64) -> H2Result<()> {
        let q_name = self.queue.get_queue_name();
        let map_name = format!("queue_offsets_{}", q_name.to_lowercase());
        if let Some(inner) = tx.inner_tx() {
            inner.put(&map_name, self.group_id.as_bytes().to_vec(), offset.to_le_bytes().to_vec())?;
        }
        Ok(())
    }

    /// 保存されているコンシューマグループのオフセットを読み込み
    pub fn load_committed_offset(&self) -> H2Result<Option<u64>> {
        let q_name = self.queue.get_queue_name();
        let map_name = format!("queue_offsets_{}", q_name.to_lowercase());
        let tx = self.session_conn.transaction()?;
        if let Some(inner) = tx.inner_tx() {
            if let Some(bytes) = inner.get(&map_name, self.group_id.as_bytes())? {
                if bytes.len() == 8 {
                    let off = u64::from_le_bytes(bytes.as_slice().try_into().unwrap());
                    return Ok(Some(off));
                }
            }
        }
        Ok(None)
    }

    fn try_receive_single(&self, tx: &Transaction, q_name: &str) -> H2Result<Option<JmsTextMessage>> {
        let curr_off = self.current_offset.load(Ordering::SeqCst);

        // 保持範囲の確認 (Head Truncation チェック)
        let min_rows = tx.query(&format!("SELECT MIN(_offset) FROM {};", q_name))?;
        if let Some(row) = min_rows.first() {
            if let Some(Value::BigInt(min_off)) = row.values.first() {
                let min_u64 = *min_off as u64;
                if min_u64 > 0 && curr_off < min_u64 {
                    return Err(H2Error::OffsetOutOfRange(format!(
                        "Requested offset {} is out of range. Oldest available offset in queue '{}' is {}",
                        curr_off, q_name, min_u64
                    )));
                }
            }
        }

        let sql = format!(
            "SELECT * FROM {} WHERE _offset >= {} ORDER BY _offset ASC LIMIT 1;",
            q_name, curr_off
        );

        let rows = tx.query(&sql)?;
        let row = match rows.into_iter().next() {
            Some(r) => r,
            None => return Ok(None),
        };

        let mut msg = JmsTextMessage::new("");
        msg.set_destination(q_name);

        if let Some(Value::BigInt(off)) = row.values.get(0) {
            msg.offset = *off as u64;
        }
        if let Some(Value::Timestamp(ts)) = row.values.get(1) {
            msg.timestamp_ms = ts.timestamp_millis() as u64;
        }
        if let Some(Value::String(mid)) = row.values.get(2) {
            msg.jms_message_id = mid.clone();
        }
        if let Some(Value::String(cid)) = row.values.get(3) {
            msg.jms_correlation_id = Some(cid.clone());
        }

        if let Some(val) = row.values.get(4) {
            match val {
                Value::String(s) => msg.set_text(s.clone()),
                Value::Json(j) => msg.set_text(j.to_string()),
                _ => msg.set_text(val.to_string()),
            }
        }

        self.current_offset.store(msg.offset + 1, Ordering::SeqCst);
        Ok(Some(msg))
    }

    /// メッセージを 1 件受信（タイムアウト指定待機付き）
    pub fn receive(&self, timeout: Option<Duration>) -> H2Result<Option<JmsTextMessage>> {
        let start = std::time::Instant::now();
        let q_name = self.queue.get_queue_name();

        loop {
            let msg_opt = if self.is_transacted {
                let mut guard = self.active_tx.lock();
                if guard.is_none() {
                    *guard = Some(self.session_conn.transaction()?);
                }
                let tx = guard.as_ref().unwrap();
                self.try_receive_single(tx, q_name)?
            } else {
                let tx = self.session_conn.transaction()?;
                let msg = self.try_receive_single(&tx, q_name)?;
                if self.ack_mode == AcknowledgeMode::AutoAcknowledge {
                    if let Some(ref m) = msg {
                        self.commit_offset_with_tx(&tx, m.get_offset() + 1)?;
                    }
                }
                tx.commit()?;
                msg
            };

            if let Some(msg) = msg_opt {
                return Ok(Some(msg));
            }

            if let Some(to) = timeout {
                if start.elapsed() >= to {
                    return Ok(None);
                }
                let remaining = to.saturating_sub(start.elapsed());
                let sleep_dur = Duration::from_millis(15).min(remaining);
                std::thread::sleep(sleep_dur);
            } else {
                return Ok(None);
            }
        }
    }

    /// ブロッキング待機なしでメッセージを 1 件受信
    pub fn receive_no_wait(&self) -> H2Result<Option<JmsTextMessage>> {
        self.receive(None)
    }

    /// 最大 max_messages 件を一括取得（Kafka poll 相当）
    pub fn receive_batch(&self, max_messages: usize, timeout: Option<Duration>) -> H2Result<Vec<JmsTextMessage>> {
        let mut messages = Vec::with_capacity(max_messages);
        let start = std::time::Instant::now();

        while messages.len() < max_messages {
            let remaining_time = timeout.map(|to| to.saturating_sub(start.elapsed()));
            if let Some(rem) = remaining_time {
                if rem.is_zero() && !messages.is_empty() {
                    break;
                }
            }

            match self.receive(remaining_time)? {
                Some(msg) => messages.push(msg),
                None => break,
            }
        }

        Ok(messages)
    }

    /// 指定トランザクション内でメッセージを 1 件受信（Exactly-Once パターン用）
    pub fn receive_with_tx(&self, tx: &Transaction, _timeout: Option<Duration>) -> H2Result<Option<JmsTextMessage>> {
        self.try_receive_single(tx, self.queue.get_queue_name())
    }
}

/// バックグラウンドで定期的にキューテーブルの保持ポリシー（RETENTION_TIME, MAX_BYTES）をスキャンしてクリーンアップを行うワーカー
pub struct QueueRetentionCleaner {
    stop_signal: Arc<AtomicBool>,
    thread_handle: Option<std::thread::JoinHandle<()>>,
}

impl QueueRetentionCleaner {
    /// 定期クリーナーを起動（interval 間隔で全キューテーブルを検査）
    pub fn start(conn: Connection, interval: Duration) -> Self {
        let stop_signal = Arc::new(AtomicBool::new(false));
        let stop_clone = Arc::clone(&stop_signal);

        let handle = std::thread::Builder::new()
            .name("h2-queue-retention-cleaner".to_string())
            .spawn(move || {
                while !stop_clone.load(Ordering::SeqCst) {
                    std::thread::sleep(interval.min(Duration::from_millis(50)));
                    if stop_clone.load(Ordering::SeqCst) {
                        break;
                    }

                    let tables = conn.engine().catalog().all_tables();
                    for tbl in tables {
                        if tbl.is_queue && (tbl.retention_duration_ms.is_some() || tbl.max_bytes.is_some()) {
                            if let Ok(tx) = conn.transaction() {
                                if let Some(inner) = tx.inner_tx() {
                                    let _ = conn.engine().purge_queue_retention(inner, &tbl.name);
                                    let _ = tx.commit();
                                }
                            }
                        }
                    }
                }
            })
            .expect("Failed to spawn QueueRetentionCleaner thread");

        Self {
            stop_signal,
            thread_handle: Some(handle),
        }
    }

    /// クリーナーを安全に停止
    pub fn stop(&mut self) {
        self.stop_signal.store(true, Ordering::SeqCst);
        if let Some(handle) = self.thread_handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for QueueRetentionCleaner {
    fn drop(&mut self) {
        self.stop();
    }
}
