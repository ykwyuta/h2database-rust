//! h2 - High-performance embedded and server-hybrid RDBMS in Rust
use std::path::Path;
use std::sync::Arc;

pub use h2_mvstore::{MVStore, TransactionStatus};
pub use h2_sql::{ExecutionResult, Row, SQLEngine};
pub use h2_types::{DataType, FromSql, H2Error, H2Result, Value};

#[cfg(feature = "async")]
pub mod async_conn;
#[cfg(feature = "async")]
pub use async_conn::{AsyncConnection, AsyncTransaction};

use std::time::Duration;

/// エルゴノミックなデータベース接続ハンドル
#[derive(Clone)]
pub struct Connection {
    store: Arc<MVStore>,
    engine: Arc<SQLEngine>,
    current_tx: Arc<parking_lot::Mutex<Option<h2_mvstore::Transaction>>>,
    default_query_timeout: Arc<parking_lot::RwLock<Option<Duration>>>,
}

#[derive(Debug, PartialEq, Eq)]
enum TxCommand {
    Begin,
    Commit,
    Rollback,
    SetStatementTimeout(u64),
    Other,
}

fn parse_tx_command(sql: &str) -> TxCommand {
    let trimmed = sql.trim().trim_end_matches(';').trim();
    if trimmed.eq_ignore_ascii_case("BEGIN")
        || trimmed.eq_ignore_ascii_case("BEGIN TRANSACTION")
        || trimmed.eq_ignore_ascii_case("START TRANSACTION")
    {
        TxCommand::Begin
    } else if trimmed.eq_ignore_ascii_case("COMMIT")
        || trimmed.eq_ignore_ascii_case("COMMIT TRANSACTION")
        || trimmed.eq_ignore_ascii_case("END")
    {
        TxCommand::Commit
    } else if trimmed.eq_ignore_ascii_case("ROLLBACK")
        || trimmed.eq_ignore_ascii_case("ROLLBACK TRANSACTION")
    {
        TxCommand::Rollback
    } else if let Some(ms) = parse_set_timeout(trimmed) {
        TxCommand::SetStatementTimeout(ms)
    } else {
        TxCommand::Other
    }
}

fn parse_set_timeout(sql: &str) -> Option<u64> {
    let parts: Vec<&str> = sql.split_whitespace().collect();
    if parts.len() >= 3 && parts[0].eq_ignore_ascii_case("SET") {
        let var = parts[1].to_ascii_lowercase();
        if var == "statement_timeout" || var == "query_timeout" {
            let val_str = parts.iter().skip(2)
                .filter(|&&s| s != "=" && !s.eq_ignore_ascii_case("TO"))
                .copied()
                .collect::<Vec<_>>()
                .join("");
            let val_clean = val_str.trim_matches('\'').trim_end_matches("ms");
            return val_clean.parse::<u64>().ok();
        }
    }
    None
}


/// エルゴノミックなパラメータ構築マクロ
#[macro_export]
macro_rules! params {
    ($($x:expr),* $(,)?) => {
        vec![$($crate::Value::from($x)),*]
    };
}

/// SQL文内のプレースホルダー ($1, ?1, ?) にパラメータをバインド
pub fn bind_params(sql: &str, params: &[Value]) -> H2Result<String> {
    if params.is_empty() {
        return Ok(sql.to_string());
    }

    let mut result = String::with_capacity(sql.len() + params.len() * 16);
    let chars: Vec<char> = sql.chars().collect();
    let mut i = 0;
    let mut auto_idx = 0;

    while i < chars.len() {
        let ch = chars[i];
        if ch == '\'' {
            result.push(ch);
            i += 1;
            while i < chars.len() {
                let c = chars[i];
                result.push(c);
                if c == '\'' {
                    if i + 1 < chars.len() && chars[i + 1] == '\'' {
                        i += 1;
                        result.push(chars[i]);
                    } else {
                        break;
                    }
                }
                i += 1;
            }
            i += 1;
            continue;
        }

        if ch == '$' || ch == '?' {
            let mut num_str = String::new();
            let mut j = i + 1;
            while j < chars.len() && chars[j].is_ascii_digit() {
                num_str.push(chars[j]);
                j += 1;
            }

            let param_idx = if !num_str.is_empty() {
                let parsed: usize = num_str.parse().map_err(|_| {
                    H2Error::Execution(format!("Invalid parameter placeholder: {}{}", ch, num_str))
                })?;
                if parsed == 0 || parsed > params.len() {
                    return Err(H2Error::Execution(format!(
                        "Parameter index {} out of range (expected 1..={})",
                        parsed,
                        params.len()
                    )));
                }
                i = j;
                parsed - 1
            } else if ch == '?' {
                if auto_idx >= params.len() {
                    return Err(H2Error::Execution(format!(
                        "Too few parameters provided: expected more than {}",
                        auto_idx
                    )));
                }
                let idx = auto_idx;
                auto_idx += 1;
                i += 1;
                idx
            } else {
                result.push(ch);
                i += 1;
                continue;
            };

            let val = &params[param_idx];
            result.push_str(&value_to_sql_literal(val));
            continue;
        }

        result.push(ch);
        i += 1;
    }

    Ok(result)
}

fn value_to_sql_literal(val: &Value) -> String {
    match val {
        Value::Null => "NULL".to_string(),
        Value::Boolean(b) => if *b { "TRUE".to_string() } else { "FALSE".to_string() },
        Value::TinyInt(n) => n.to_string(),
        Value::SmallInt(n) => n.to_string(),
        Value::Integer(n) => n.to_string(),
        Value::BigInt(n) => n.to_string(),
        Value::Float(n) => n.to_string(),
        Value::Double(n) => n.to_string(),
        Value::Decimal(d) => d.to_string(),
        Value::String(s) => format!("'{}'", s.replace('\'', "''")),
        Value::Uuid(u) => format!("'{}'", u),
        Value::Date(d) => format!("'{}'", d),
        Value::Time(t) => format!("'{}'", t),
        Value::Timestamp(dt) => format!("'{}'", dt),
        Value::Json(j) => format!("'{}'", j.to_string().replace('\'', "''")),
        Value::Bytes(b) => format!("'{}'", String::from_utf8_lossy(b).replace('\'', "''")),
        Value::Array(arr) => format!(
            "[{}]",
            arr.iter().map(value_to_sql_literal).collect::<Vec<_>>().join(", ")
        ),
    }
}

impl Connection {
    /// ファイルベースのデータベースを開く（存在しない場合は自動生成）
    pub fn open<P: AsRef<Path>>(path: P) -> H2Result<Self> {
        let store = Arc::new(MVStore::open(path)?);
        let engine = Arc::new(SQLEngine::new(Arc::clone(&store))?);
        Ok(Self {
            store,
            engine,
            current_tx: Arc::new(parking_lot::Mutex::new(None)),
            default_query_timeout: Arc::new(parking_lot::RwLock::new(None)),
        })
    }

    /// インメモリデータベースを開く
    pub fn open_in_memory() -> H2Result<Self> {
        let store = Arc::new(MVStore::open_in_memory());
        let engine = Arc::new(SQLEngine::new(Arc::clone(&store))?);
        Ok(Self {
            store,
            engine,
            current_tx: Arc::new(parking_lot::Mutex::new(None)),
            default_query_timeout: Arc::new(parking_lot::RwLock::new(None)),
        })
    }

    /// 同一のデータベースを共有し、独立したトランザクション状態を持つ新しいセッション（接続）を作成
    pub fn new_session(&self) -> Self {
        Self {
            store: Arc::clone(&self.store),
            engine: Arc::clone(&self.engine),
            current_tx: Arc::new(parking_lot::Mutex::new(None)),
            default_query_timeout: Arc::new(parking_lot::RwLock::new(*self.default_query_timeout.read())),
        }
    }

    /// ロック待機タイムアウト時間（ミリ秒）を設定
    pub fn set_lock_timeout_ms(&self, ms: u64) {
        self.engine.tx_store().set_lock_timeout_ms(ms);
    }

    /// セッション全体のデフォルトクエリタイムアウトを設定（None または 0 で無制限）
    pub fn set_query_timeout(&self, timeout: Option<Duration>) {
        *self.default_query_timeout.write() = timeout;
    }

    /// セッション全体のデフォルトクエリタイムアウトをミリ秒単位で設定（0 で無制限）
    pub fn set_query_timeout_ms(&self, ms: u64) {
        let timeout = if ms == 0 { None } else { Some(Duration::from_millis(ms)) };
        self.set_query_timeout(timeout);
    }

    /// 現在設定されているデフォルトクエリタイムアウトを取得
    pub fn query_timeout_duration(&self) -> Option<Duration> {
        *self.default_query_timeout.read()
    }

    fn setup_timeout_guard(&self) -> Option<h2_types::TimeoutGuard> {
        if h2_types::remaining_query_timeout().is_none() {
            if let Some(timeout) = *self.default_query_timeout.read() {
                return Some(h2_types::set_query_timeout(Some(timeout)));
            }
        }
        None
    }

    /// DDLやDML文、または明示的トランザクション制御文（BEGIN/COMMIT/ROLLBACK/SET）を実行
    pub fn execute(&self, sql: &str) -> H2Result<u64> {
        let _guard = self.setup_timeout_guard();
        match parse_tx_command(sql) {
            TxCommand::Begin => {
                let mut guard = self.current_tx.lock();
                if guard.is_some() {
                    return Err(H2Error::Transaction(
                        "Transaction is already in progress".to_string(),
                    ));
                }
                *guard = Some(self.engine.tx_store().begin());
                Ok(0)
            }
            TxCommand::Commit => {
                let mut guard = self.current_tx.lock();
                let tx = guard.take().ok_or_else(|| {
                    H2Error::Transaction("No active transaction to commit".to_string())
                })?;
                tx.commit()?;
                Ok(0)
            }
            TxCommand::Rollback => {
                let mut guard = self.current_tx.lock();
                let tx = guard.take().ok_or_else(|| {
                    H2Error::Transaction("No active transaction to rollback".to_string())
                })?;
                tx.rollback()?;
                Ok(0)
            }
            TxCommand::SetStatementTimeout(ms) => {
                self.set_query_timeout_ms(ms);
                Ok(0)
            }
            TxCommand::Other => {
                let guard = self.current_tx.lock();
                let res = if let Some(ref tx) = *guard {
                    self.engine.execute_with_tx(tx, sql)?
                } else {
                    self.engine.execute(sql)?
                };
                match res {
                    ExecutionResult::Ddl => Ok(0),
                    ExecutionResult::Dml { affected_rows } => Ok(affected_rows),
                    ExecutionResult::Query { .. } => {
                        Err(H2Error::Execution("Use query() for SELECT statements".to_string()))
                    }
                }
            }
        }
    }

    /// タイムアウトを指定して DDL/DML 文を実行
    pub fn execute_timeout(&self, sql: &str, timeout: Duration) -> H2Result<u64> {
        let _guard = h2_types::set_query_timeout(Some(timeout));
        self.execute(sql)
    }

    /// パラメータ付きで DDL/DML 文を実行
    pub fn execute_params(&self, sql: &str, params: &[Value]) -> H2Result<u64> {
        let _guard = self.setup_timeout_guard();
        let bound = bind_params(sql, params)?;
        self.execute(&bound)
    }

    /// タイムアウトおよびパラメータを指定して DDL/DML 文を実行
    pub fn execute_params_timeout(&self, sql: &str, params: &[Value], timeout: Duration) -> H2Result<u64> {
        let _guard = h2_types::set_query_timeout(Some(timeout));
        self.execute_params(sql, params)
    }

    /// クエリ（SELECT）文を実行し、行リストを返す
    pub fn query(&self, sql: &str) -> H2Result<Vec<Row>> {
        let _guard = self.setup_timeout_guard();
        let guard = self.current_tx.lock();
        let res = if let Some(ref tx) = *guard {
            self.engine.execute_with_tx(tx, sql)?
        } else {
            self.engine.execute(sql)?
        };
        match res {
            ExecutionResult::Query { rows, .. } => Ok(rows),
            _ => Err(H2Error::Execution("Use execute() for DDL/DML statements".to_string())),
        }
    }

    /// タイムアウトを指定してクエリ（SELECT）文を実行
    pub fn query_timeout(&self, sql: &str, timeout: Duration) -> H2Result<Vec<Row>> {
        let _guard = h2_types::set_query_timeout(Some(timeout));
        self.query(sql)
    }

    /// 現在明示的トランザクション中（BEGIN実行後、未COMMIT/ROLLBACK）であるか確認
    pub fn in_transaction(&self) -> bool {
        self.current_tx.lock().is_some()
    }

    /// パラメータ付きでクエリを実行
    pub fn query_params(&self, sql: &str, params: &[Value]) -> H2Result<Vec<Row>> {
        let _guard = self.setup_timeout_guard();
        let bound = bind_params(sql, params)?;
        self.query(&bound)
    }

    /// タイムアウトおよびパラメータを指定してクエリを実行
    pub fn query_params_timeout(&self, sql: &str, params: &[Value], timeout: Duration) -> H2Result<Vec<Row>> {
        let _guard = h2_types::set_query_timeout(Some(timeout));
        self.query_params(sql, params)
    }

    /// 新規トランザクションを開始（スナップショット分離）
    pub fn transaction(&self) -> H2Result<Transaction> {
        let inner_tx = self.engine.tx_store().begin();
        Ok(Transaction {
            inner: Some(inner_tx),
            engine: Arc::clone(&self.engine),
            default_query_timeout: *self.default_query_timeout.read(),
        })
    }

    /// 基礎となるストレージエンジンの現在のバージョン番号を取得
    pub fn version(&self) -> u64 {
        self.store.current_version()
    }

    /// ストレージのコンパクション（Vacuum）を実行し、古い死にチャンクを破棄してファイルを縮小
    pub fn vacuum(&self) -> H2Result<()> {
        self.store.compact()
    }

    #[cfg(feature = "server")]
    /// バックグラウンドで PostgreSQL 互換ワイヤプロトコルサーバーを起動し、リッスンアドレスを返却
    pub async fn start_pg_server(&self, addr: std::net::SocketAddr) -> H2Result<std::net::SocketAddr> {
        let server = h2_server::PgServer::bind(addr, Arc::clone(&self.engine)).await?;
        let local_addr = server.local_addr()?;
        tokio::spawn(async move {
            let _ = server.run().await;
        });
        Ok(local_addr)
    }
}

/// rusqlite 風の型安全なトランザクションハンドル
pub struct Transaction {
    inner: Option<h2_mvstore::Transaction>,
    engine: Arc<SQLEngine>,
    default_query_timeout: Option<Duration>,
}

impl Transaction {
    fn setup_timeout_guard(&self) -> Option<h2_types::TimeoutGuard> {
        if h2_types::remaining_query_timeout().is_none() {
            if let Some(timeout) = self.default_query_timeout {
                return Some(h2_types::set_query_timeout(Some(timeout)));
            }
        }
        None
    }

    /// トランザクション内で DDL/DML 文を実行
    pub fn execute(&self, sql: &str) -> H2Result<u64> {
        let _guard = self.setup_timeout_guard();
        let tx = self.inner.as_ref().ok_or_else(|| {
            H2Error::Transaction("Transaction is already closed".to_string())
        })?;

        match self.engine.execute_with_tx(tx, sql)? {
            ExecutionResult::Ddl => Ok(0),
            ExecutionResult::Dml { affected_rows } => Ok(affected_rows),
            ExecutionResult::Query { .. } => {
                Err(H2Error::Execution("Use query() for SELECT statements".to_string()))
            }
        }
    }

    /// タイムアウトを指定してトランザクション内で DDL/DML 文を実行
    pub fn execute_timeout(&self, sql: &str, timeout: Duration) -> H2Result<u64> {
        let _guard = h2_types::set_query_timeout(Some(timeout));
        self.execute(sql)
    }

    /// トランザクション内でパラメータ付き DDL/DML 文を実行
    pub fn execute_params(&self, sql: &str, params: &[Value]) -> H2Result<u64> {
        let _guard = self.setup_timeout_guard();
        let bound = bind_params(sql, params)?;
        self.execute(&bound)
    }

    /// タイムアウトおよびパラメータを指定してトランザクション内で DDL/DML 文を実行
    pub fn execute_params_timeout(&self, sql: &str, params: &[Value], timeout: Duration) -> H2Result<u64> {
        let _guard = h2_types::set_query_timeout(Some(timeout));
        self.execute_params(sql, params)
    }

    /// トランザクション内でクエリ（SELECT）を実行
    pub fn query(&self, sql: &str) -> H2Result<Vec<Row>> {
        let _guard = self.setup_timeout_guard();
        let tx = self.inner.as_ref().ok_or_else(|| {
            H2Error::Transaction("Transaction is already closed".to_string())
        })?;

        match self.engine.execute_with_tx(tx, sql)? {
            ExecutionResult::Query { rows, .. } => Ok(rows),
            _ => Err(H2Error::Execution("Use execute() for DDL/DML statements".to_string())),
        }
    }

    /// タイムアウトを指定してトランザクション内でクエリを実行
    pub fn query_timeout(&self, sql: &str, timeout: Duration) -> H2Result<Vec<Row>> {
        let _guard = h2_types::set_query_timeout(Some(timeout));
        self.query(sql)
    }

    /// トランザクション内でパラメータ付きクエリを実行
    pub fn query_params(&self, sql: &str, params: &[Value]) -> H2Result<Vec<Row>> {
        let _guard = self.setup_timeout_guard();
        let bound = bind_params(sql, params)?;
        self.query(&bound)
    }

    /// タイムアウトおよびパラメータを指定してトランザクション内でクエリを実行
    pub fn query_params_timeout(&self, sql: &str, params: &[Value], timeout: Duration) -> H2Result<Vec<Row>> {
        let _guard = h2_types::set_query_timeout(Some(timeout));
        self.query_params(sql, params)
    }

    /// トランザクションをコミットして変更を確定
    pub fn commit(mut self) -> H2Result<()> {
        if let Some(tx) = self.inner.take() {
            tx.commit()?;
        }
        Ok(())
    }

    /// トランザクションをロールバックして変更を取り消し
    pub fn rollback(mut self) -> H2Result<()> {
        if let Some(tx) = self.inner.take() {
            tx.rollback()?;
        }
        Ok(())
    }

    pub fn tx_id(&self) -> Option<u64> {
        self.inner.as_ref().map(|tx| tx.tx_id)
    }

    pub fn snapshot_version(&self) -> Option<u64> {
        self.inner.as_ref().map(|tx| tx.snapshot_version)
    }
}

impl Drop for Transaction {
    fn drop(&mut self) {
        // commit() が明示的に呼ばれずにスコープを抜けた場合、自動でロールバックされる (RAII)
        if let Some(tx) = self.inner.take() {
            let _ = tx.rollback();
        }
    }
}

pub type Result<T> = H2Result<T>;

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use tempfile::NamedTempFile;

    #[test]
    fn test_embedded_connection_crud() {
        let conn = Connection::open_in_memory().unwrap();

        // 1. CREATE TABLE
        let res = conn.execute(
            "CREATE TABLE products (
                id INTEGER PRIMARY KEY,
                name VARCHAR,
                price DECIMAL(10, 2)
            )",
        ).unwrap();
        assert_eq!(res, 0);

        // 2. INSERT
        let inserted = conn.execute(
            "INSERT INTO products VALUES (1, 'Mechanical Keyboard', 120.00), (2, 'Ergonomic Mouse', 65.50)",
        ).unwrap();
        assert_eq!(inserted, 2);

        // 3. SELECT
        let rows = conn.query("SELECT id, name, price FROM products WHERE price > 70").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get(1), Some(&Value::String("Mechanical Keyboard".to_string())));
    }

    #[test]
    fn test_file_persisted_connection() {
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path().to_path_buf();

        // 1. 初回接続とデータ投入
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute("CREATE TABLE items (id INTEGER, title VARCHAR)").unwrap();
            conn.execute("INSERT INTO items VALUES (1, 'Book A'), (2, 'Book B')").unwrap();
        }

        // 2. 再オープンして永続化確認
        {
            let conn = Connection::open(&path).unwrap();
            let rows = conn.query("SELECT * FROM items").unwrap();
            assert_eq!(rows.len(), 2);
            assert_eq!(rows[0].get(1), Some(&Value::String("Book A".to_string())));
            assert_eq!(rows[1].get(1), Some(&Value::String("Book B".to_string())));
        }
    }

    #[test]
    fn test_transaction_commit_and_rollback() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute("CREATE TABLE users (id INTEGER, name VARCHAR)").unwrap();
        conn.execute("INSERT INTO users VALUES (1, 'Alice')").unwrap();

        // 1. 明示的ロールバック
        let tx = conn.transaction().unwrap();
        tx.execute("INSERT INTO users VALUES (2, 'Bob')").unwrap();
        assert_eq!(tx.query("SELECT * FROM users").unwrap().len(), 2);
        tx.rollback().unwrap();

        // ロールバック後は1件のみ
        assert_eq!(conn.query("SELECT * FROM users").unwrap().len(), 1);

        // 2. スコープ離脱による自動ロールバック (RAII)
        {
            let tx2 = conn.transaction().unwrap();
            tx2.execute("INSERT INTO users VALUES (3, 'Charlie')").unwrap();
            // commit() を呼ばずにブロック終了
        }

        // 自動ロールバックされたため、やはり1件のみ
        assert_eq!(conn.query("SELECT * FROM users").unwrap().len(), 1);

        // 3. コミットの反映
        let tx3 = conn.transaction().unwrap();
        tx3.execute("INSERT INTO users VALUES (4, 'David')").unwrap();
        tx3.commit().unwrap();

        assert_eq!(conn.query("SELECT * FROM users").unwrap().len(), 2);
    }

    #[test]
    fn test_concurrent_readers_and_writers() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute("CREATE TABLE counter (id INTEGER, count INTEGER)").unwrap();
        conn.execute("INSERT INTO counter VALUES (1, 100)").unwrap();

        let conn1 = conn.clone();
        let conn2 = conn.clone();

        // スレッド1: トランザクションを開始し、更新を行うがコミット前
        let tx1 = conn1.transaction().unwrap();
        tx1.execute("INSERT INTO counter VALUES (2, 200)").unwrap();

        // スレッド2: 別トランザクションで読み取り（リーダーは待たされない！）
        let handle = thread::spawn(move || {
            let tx2 = conn2.transaction().unwrap();
            let rows = tx2.query("SELECT * FROM counter").unwrap();
            // tx1 の未コミット変更は見えず、初期の1件のみが見える (Snapshot Isolation)
            assert_eq!(rows.len(), 1);
            tx2.commit().unwrap();
        });

        handle.join().unwrap();

        // tx1 をコミット
        tx1.commit().unwrap();

        // コミット後は2件見える
        assert_eq!(conn.query("SELECT * FROM counter").unwrap().len(), 2);
    }

    #[cfg(feature = "server")]
    #[tokio::test]
    async fn test_hybrid_embedded_and_pgwire() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpStream;

        let conn = Connection::open_in_memory().unwrap();
        // 組み込みAPIでテーブル作成とデータ挿入
        conn.execute("CREATE TABLE products (id INTEGER, name VARCHAR)").unwrap();
        conn.execute("INSERT INTO products VALUES (1, 'Laptop')").unwrap();

        // バックグラウンドでPostgreSQL互換サーバーを起動
        let server_addr = conn
            .start_pg_server("127.0.0.1:0".parse().unwrap())
            .await
            .unwrap();

        // 外部クライアント（psqlやDBeaver相当）から接続
        let mut client = TcpStream::connect(server_addr).await.unwrap();

        // 簡易ハンドシェイク（SSLRequest + StartupMessage）
        let mut ssl_req = Vec::new();
        ssl_req.extend_from_slice(&8u32.to_be_bytes());
        ssl_req.extend_from_slice(&80877103u32.to_be_bytes());
        client.write_all(&ssl_req).await.unwrap();

        let mut ssl_resp = [0u8; 1];
        client.read_exact(&mut ssl_resp).await.unwrap();

        let mut startup = Vec::new();
        startup.extend_from_slice(&196608u32.to_be_bytes());
        startup.extend_from_slice(b"user\0app\0database\0main\0\0");
        let startup_len = 4 + startup.len() as u32;

        let mut startup_msg = Vec::new();
        startup_msg.extend_from_slice(&startup_len.to_be_bytes());
        startup_msg.extend_from_slice(&startup);
        client.write_all(&startup_msg).await.unwrap();

        let mut init_buf = vec![0u8; 1024];
        client.read(&mut init_buf).await.unwrap();

        // 外部クライアントからクエリを発行して、組み込みAPIで挿入したデータを参照
        let sql = "SELECT * FROM products";
        let sql_bytes = sql.as_bytes();
        let len = 4 + sql_bytes.len() + 1;
        let mut query_buf = Vec::new();
        query_buf.push(b'Q');
        query_buf.extend_from_slice(&(len as u32).to_be_bytes());
        query_buf.extend_from_slice(sql_bytes);
        query_buf.push(0);
        client.write_all(&query_buf).await.unwrap();

        let mut resp_buf = vec![0u8; 1024];
        let n = client.read(&mut resp_buf).await.unwrap();
        let resp_str = String::from_utf8_lossy(&resp_buf[..n]);
        assert!(resp_str.contains("Laptop"));

        // 外部クライアントからデータを追加挿入
        let insert_sql = "INSERT INTO products VALUES (2, 'Smartphone')";
        let insert_bytes = insert_sql.as_bytes();
        let len = 4 + insert_bytes.len() + 1;
        let mut insert_buf = Vec::new();
        insert_buf.push(b'Q');
        insert_buf.extend_from_slice(&(len as u32).to_be_bytes());
        insert_buf.extend_from_slice(insert_bytes);
        insert_buf.push(0);
        client.write_all(&insert_buf).await.unwrap();
        client.read(&mut resp_buf).await.unwrap();

        // 組み込みAPI側から即座に参照可能であることを確認
        let rows = conn.query("SELECT * FROM products").unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].get(1), Some(&Value::String("Smartphone".to_string())));
    }

    #[test]
    fn test_compaction_and_vacuum() {
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path().to_path_buf();

        let conn = Connection::open(&path).unwrap();
        conn.execute("CREATE TABLE records (id INTEGER, content VARCHAR)").unwrap();

        // 1. 200件のデータを挿入（チャンク追記書き込みが発生）
        for i in 1..=200 {
            let sql = format!("INSERT INTO records VALUES ({}, 'This is a long record content number {}')", i, i);
            conn.execute(&sql).unwrap();
        }

        let pre_delete_size = std::fs::metadata(&path).unwrap().len();

        // 2. 180件を削除（死にページが発生）
        conn.execute("DELETE FROM records WHERE id > 20").unwrap();

        // 3. VACUUM を実行してコンパクション
        conn.vacuum().unwrap();

        let post_vacuum_size = std::fs::metadata(&path).unwrap().len();

        // コンパクションによりファイルサイズが縮小していることを検証
        assert!(post_vacuum_size < pre_delete_size, "Expected {} < {}", post_vacuum_size, pre_delete_size);

        // 残り20件が正確に読み出せることを検証
        let remaining = conn.query("SELECT * FROM records").unwrap();
        assert_eq!(remaining.len(), 20);
    }
}

