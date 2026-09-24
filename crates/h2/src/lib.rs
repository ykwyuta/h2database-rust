//! h2 - High-performance embedded and server-hybrid RDBMS in Rust
use std::path::Path;
use std::sync::Arc;

pub use h2_mvstore::{MVStore, TransactionStatus};
pub use h2_sql::{ExecutionResult, Row, SQLEngine};
pub use h2_types::{DataType, H2Error, H2Result, Value};

/// エルゴノミックなデータベース接続ハンドル
#[derive(Clone)]
pub struct Connection {
    store: Arc<MVStore>,
    engine: Arc<SQLEngine>,
}

impl Connection {
    /// ファイルベースのデータベースを開く（存在しない場合は自動生成）
    pub fn open<P: AsRef<Path>>(path: P) -> H2Result<Self> {
        let store = Arc::new(MVStore::open(path)?);
        let engine = Arc::new(SQLEngine::new(Arc::clone(&store))?);
        Ok(Self { store, engine })
    }

    /// インメモリデータベースを開く
    pub fn open_in_memory() -> H2Result<Self> {
        let store = Arc::new(MVStore::open_in_memory());
        let engine = Arc::new(SQLEngine::new(Arc::clone(&store))?);
        Ok(Self { store, engine })
    }

    /// DDLやDML（INSERT/UPDATE/DELETE）文を実行し、影響行数を返す
    pub fn execute(&self, sql: &str) -> H2Result<u64> {
        match self.engine.execute(sql)? {
            ExecutionResult::Ddl => Ok(0),
            ExecutionResult::Dml { affected_rows } => Ok(affected_rows),
            ExecutionResult::Query { .. } => {
                Err(H2Error::Execution("Use query() for SELECT statements".to_string()))
            }
        }
    }

    /// クエリ（SELECT）文を実行し、行リストを返す
    pub fn query(&self, sql: &str) -> H2Result<Vec<Row>> {
        match self.engine.execute(sql)? {
            ExecutionResult::Query { rows, .. } => Ok(rows),
            _ => Err(H2Error::Execution("Use execute() for DDL/DML statements".to_string())),
        }
    }

    /// 新規トランザクションを開始（スナップショット分離）
    pub fn transaction(&self) -> H2Result<Transaction> {
        let inner_tx = self.engine.tx_store().begin();
        Ok(Transaction {
            inner: Some(inner_tx),
            engine: Arc::clone(&self.engine),
        })
    }

    /// 基礎となるストレージエンジンの現在のバージョン番号を取得
    pub fn version(&self) -> u64 {
        self.store.current_version()
    }
}

/// rusqlite 風の型安全なトランザクションハンドル
pub struct Transaction {
    inner: Option<h2_mvstore::Transaction>,
    engine: Arc<SQLEngine>,
}

impl Transaction {
    /// トランザクション内で DDL/DML 文を実行
    pub fn execute(&self, sql: &str) -> H2Result<u64> {
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

    /// トランザクション内でクエリ（SELECT）を実行
    pub fn query(&self, sql: &str) -> H2Result<Vec<Row>> {
        let tx = self.inner.as_ref().ok_or_else(|| {
            H2Error::Transaction("Transaction is already closed".to_string())
        })?;

        match self.engine.execute_with_tx(tx, sql)? {
            ExecutionResult::Query { rows, .. } => Ok(rows),
            _ => Err(H2Error::Execution("Use execute() for DDL/DML statements".to_string())),
        }
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
}
