//! Tokio ネイティブな非同期データベース接続インターフェース
use std::path::Path;
use std::sync::Arc;
use h2_types::{H2Error, H2Result, Value};
use crate::{Connection, Row, Transaction};

/// 非同期データベース接続ハンドル (Tokio ネイティブ)
#[derive(Clone)]
pub struct AsyncConnection {
    conn: Connection,
}

impl AsyncConnection {
    /// ファイルベースのデータベースを非同期に開く
    pub async fn open<P: AsRef<Path> + Send + 'static>(path: P) -> H2Result<Self> {
        let conn = tokio::task::spawn_blocking(move || Connection::open(path))
            .await
            .map_err(|e| H2Error::Execution(format!("Tokio spawn_blocking error: {}", e)))??;
        Ok(Self { conn })
    }

    /// インメモリデータベースを非同期に開く
    pub async fn open_in_memory() -> H2Result<Self> {
        let conn = tokio::task::spawn_blocking(Connection::open_in_memory)
            .await
            .map_err(|e| H2Error::Execution(format!("Tokio spawn_blocking error: {}", e)))??;
        Ok(Self { conn })
    }

    /// DDLやDML（INSERT/UPDATE/DELETE）文、またはトランザクション制御文を非同期に実行
    pub async fn execute(&self, sql: &str) -> H2Result<u64> {
        let conn = self.conn.clone();
        let sql = sql.to_string();
        tokio::task::spawn_blocking(move || conn.execute(&sql))
            .await
            .map_err(|e| H2Error::Execution(format!("Tokio spawn_blocking error: {}", e)))?
    }

    /// タイムアウトを指定して DDL/DML 文を非同期に実行
    pub async fn execute_timeout(&self, sql: &str, timeout: std::time::Duration) -> H2Result<u64> {
        let conn = self.conn.clone();
        let sql = sql.to_string();
        tokio::task::spawn_blocking(move || conn.execute_timeout(&sql, timeout))
            .await
            .map_err(|e| H2Error::Execution(format!("Tokio spawn_blocking error: {}", e)))?
    }

    /// パラメータ付きで DDL/DML 文を非同期に実行
    pub async fn execute_params(&self, sql: &str, params: &[Value]) -> H2Result<u64> {
        let conn = self.conn.clone();
        let sql = sql.to_string();
        let params = params.to_vec();
        tokio::task::spawn_blocking(move || conn.execute_params(&sql, &params))
            .await
            .map_err(|e| H2Error::Execution(format!("Tokio spawn_blocking error: {}", e)))?
    }

    /// タイムアウトおよびパラメータを指定して DDL/DML 文を非同期に実行
    pub async fn execute_params_timeout(&self, sql: &str, params: &[Value], timeout: std::time::Duration) -> H2Result<u64> {
        let conn = self.conn.clone();
        let sql = sql.to_string();
        let params = params.to_vec();
        tokio::task::spawn_blocking(move || conn.execute_params_timeout(&sql, &params, timeout))
            .await
            .map_err(|e| H2Error::Execution(format!("Tokio spawn_blocking error: {}", e)))?
    }

    /// クエリ（SELECT）文を非同期に実行し、行リストを返す
    pub async fn query(&self, sql: &str) -> H2Result<Vec<Row>> {
        let conn = self.conn.clone();
        let sql = sql.to_string();
        tokio::task::spawn_blocking(move || conn.query(&sql))
            .await
            .map_err(|e| H2Error::Execution(format!("Tokio spawn_blocking error: {}", e)))?
    }

    /// タイムアウトを指定してクエリ（SELECT）文を非同期に実行
    pub async fn query_timeout(&self, sql: &str, timeout: std::time::Duration) -> H2Result<Vec<Row>> {
        let conn = self.conn.clone();
        let sql = sql.to_string();
        tokio::task::spawn_blocking(move || conn.query_timeout(&sql, timeout))
            .await
            .map_err(|e| H2Error::Execution(format!("Tokio spawn_blocking error: {}", e)))?
    }

    /// パラメータ付きでクエリを非同期に実行
    pub async fn query_params(&self, sql: &str, params: &[Value]) -> H2Result<Vec<Row>> {
        let conn = self.conn.clone();
        let sql = sql.to_string();
        let params = params.to_vec();
        tokio::task::spawn_blocking(move || conn.query_params(&sql, &params))
            .await
            .map_err(|e| H2Error::Execution(format!("Tokio spawn_blocking error: {}", e)))?
    }

    /// タイムアウトおよびパラメータを指定してクエリを非同期に実行
    pub async fn query_params_timeout(&self, sql: &str, params: &[Value], timeout: std::time::Duration) -> H2Result<Vec<Row>> {
        let conn = self.conn.clone();
        let sql = sql.to_string();
        let params = params.to_vec();
        tokio::task::spawn_blocking(move || conn.query_params_timeout(&sql, &params, timeout))
            .await
            .map_err(|e| H2Error::Execution(format!("Tokio spawn_blocking error: {}", e)))?
    }

    /// セッション全体のデフォルトクエリタイムアウトを設定
    pub fn set_query_timeout(&self, timeout: Option<std::time::Duration>) {
        self.conn.set_query_timeout(timeout);
    }

    /// セッション全体のデフォルトクエリタイムアウトをミリ秒単位で設定
    pub fn set_query_timeout_ms(&self, ms: u64) {
        self.conn.set_query_timeout_ms(ms);
    }

    /// 新規非同期トランザクションを開始
    pub async fn transaction(&self) -> H2Result<AsyncTransaction> {
        let conn = self.conn.clone();
        let tx = tokio::task::spawn_blocking(move || conn.transaction())
            .await
            .map_err(|e| H2Error::Execution(format!("Tokio spawn_blocking error: {}", e)))??;
        Ok(AsyncTransaction {
            inner: Arc::new(parking_lot::Mutex::new(Some(tx))),
        })
    }

    /// ストレージのコンパクション（Vacuum）を非同期に実行
    pub async fn vacuum(&self) -> H2Result<()> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || conn.vacuum())
            .await
            .map_err(|e| H2Error::Execution(format!("Tokio spawn_blocking error: {}", e)))?
    }

    /// 基礎となるストレージエンジンの現在のバージョン番号を取得
    pub fn version(&self) -> u64 {
        self.conn.version()
    }
}

/// 非同期トランザクションハンドル
#[derive(Clone)]
pub struct AsyncTransaction {
    inner: Arc<parking_lot::Mutex<Option<Transaction>>>,
}

impl AsyncTransaction {
    /// トランザクション内で DDL/DML 文を非同期実行
    pub async fn execute(&self, sql: &str) -> H2Result<u64> {
        let inner = Arc::clone(&self.inner);
        let sql = sql.to_string();
        tokio::task::spawn_blocking(move || {
            let guard = inner.lock();
            let tx = guard.as_ref().ok_or_else(|| {
                H2Error::Transaction("Transaction is already closed".to_string())
            })?;
            tx.execute(&sql)
        })
        .await
        .map_err(|e| H2Error::Execution(format!("Tokio spawn_blocking error: {}", e)))?
    }

    /// タイムアウトを指定してトランザクション内で DDL/DML 文を非同期実行
    pub async fn execute_timeout(&self, sql: &str, timeout: std::time::Duration) -> H2Result<u64> {
        let inner = Arc::clone(&self.inner);
        let sql = sql.to_string();
        tokio::task::spawn_blocking(move || {
            let guard = inner.lock();
            let tx = guard.as_ref().ok_or_else(|| {
                H2Error::Transaction("Transaction is already closed".to_string())
            })?;
            tx.execute_timeout(&sql, timeout)
        })
        .await
        .map_err(|e| H2Error::Execution(format!("Tokio spawn_blocking error: {}", e)))?
    }

    /// トランザクション内でパラメータ付き DDL/DML 文を非同期実行
    pub async fn execute_params(&self, sql: &str, params: &[Value]) -> H2Result<u64> {
        let inner = Arc::clone(&self.inner);
        let sql = sql.to_string();
        let params = params.to_vec();
        tokio::task::spawn_blocking(move || {
            let guard = inner.lock();
            let tx = guard.as_ref().ok_or_else(|| {
                H2Error::Transaction("Transaction is already closed".to_string())
            })?;
            tx.execute_params(&sql, &params)
        })
        .await
        .map_err(|e| H2Error::Execution(format!("Tokio spawn_blocking error: {}", e)))?
    }

    /// タイムアウトおよびパラメータを指定してトランザクション内で DDL/DML 文を非同期実行
    pub async fn execute_params_timeout(&self, sql: &str, params: &[Value], timeout: std::time::Duration) -> H2Result<u64> {
        let inner = Arc::clone(&self.inner);
        let sql = sql.to_string();
        let params = params.to_vec();
        tokio::task::spawn_blocking(move || {
            let guard = inner.lock();
            let tx = guard.as_ref().ok_or_else(|| {
                H2Error::Transaction("Transaction is already closed".to_string())
            })?;
            tx.execute_params_timeout(&sql, &params, timeout)
        })
        .await
        .map_err(|e| H2Error::Execution(format!("Tokio spawn_blocking error: {}", e)))?
    }

    /// トランザクション内でクエリ（SELECT）を非同期実行
    pub async fn query(&self, sql: &str) -> H2Result<Vec<Row>> {
        let inner = Arc::clone(&self.inner);
        let sql = sql.to_string();
        tokio::task::spawn_blocking(move || {
            let guard = inner.lock();
            let tx = guard.as_ref().ok_or_else(|| {
                H2Error::Transaction("Transaction is already closed".to_string())
            })?;
            tx.query(&sql)
        })
        .await
        .map_err(|e| H2Error::Execution(format!("Tokio spawn_blocking error: {}", e)))?
    }

    /// タイムアウトを指定してトランザクション内でクエリ（SELECT）を非同期実行
    pub async fn query_timeout(&self, sql: &str, timeout: std::time::Duration) -> H2Result<Vec<Row>> {
        let inner = Arc::clone(&self.inner);
        let sql = sql.to_string();
        tokio::task::spawn_blocking(move || {
            let guard = inner.lock();
            let tx = guard.as_ref().ok_or_else(|| {
                H2Error::Transaction("Transaction is already closed".to_string())
            })?;
            tx.query_timeout(&sql, timeout)
        })
        .await
        .map_err(|e| H2Error::Execution(format!("Tokio spawn_blocking error: {}", e)))?
    }

    /// トランザクション内でパラメータ付きクエリを非同期実行
    pub async fn query_params(&self, sql: &str, params: &[Value]) -> H2Result<Vec<Row>> {
        let inner = Arc::clone(&self.inner);
        let sql = sql.to_string();
        let params = params.to_vec();
        tokio::task::spawn_blocking(move || {
            let guard = inner.lock();
            let tx = guard.as_ref().ok_or_else(|| {
                H2Error::Transaction("Transaction is already closed".to_string())
            })?;
            tx.query_params(&sql, &params)
        })
        .await
        .map_err(|e| H2Error::Execution(format!("Tokio spawn_blocking error: {}", e)))?
    }

    /// タイムアウトおよびパラメータを指定してトランザクション内でクエリを非同期実行
    pub async fn query_params_timeout(&self, sql: &str, params: &[Value], timeout: std::time::Duration) -> H2Result<Vec<Row>> {
        let inner = Arc::clone(&self.inner);
        let sql = sql.to_string();
        let params = params.to_vec();
        tokio::task::spawn_blocking(move || {
            let guard = inner.lock();
            let tx = guard.as_ref().ok_or_else(|| {
                H2Error::Transaction("Transaction is already closed".to_string())
            })?;
            tx.query_params_timeout(&sql, &params, timeout)
        })
        .await
        .map_err(|e| H2Error::Execution(format!("Tokio spawn_blocking error: {}", e)))?
    }

    /// トランザクションをコミットして変更を確定
    pub async fn commit(self) -> H2Result<()> {
        let inner = Arc::clone(&self.inner);
        tokio::task::spawn_blocking(move || {
            let mut guard = inner.lock();
            let tx = guard.take().ok_or_else(|| {
                H2Error::Transaction("Transaction is already closed".to_string())
            })?;
            tx.commit()
        })
        .await
        .map_err(|e| H2Error::Execution(format!("Tokio spawn_blocking error: {}", e)))?
    }

    /// トランザクションをロールバックして変更を破棄
    pub async fn rollback(self) -> H2Result<()> {
        let inner = Arc::clone(&self.inner);
        tokio::task::spawn_blocking(move || {
            let mut guard = inner.lock();
            let tx = guard.take().ok_or_else(|| {
                H2Error::Transaction("Transaction is already closed".to_string())
            })?;
            tx.rollback()
        })
        .await
        .map_err(|e| H2Error::Execution(format!("Tokio spawn_blocking error: {}", e)))?
    }
}
