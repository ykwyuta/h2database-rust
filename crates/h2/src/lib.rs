//! h2 - High-performance embedded and server-hybrid RDBMS in Rust
use std::path::Path;
use std::sync::Arc;

pub use h2_mvstore::MVStore;
pub use h2_sql::{ExecutionResult, Row, SQLEngine};
pub use h2_types::{DataType, H2Error, H2Result, Value};

/// エルゴノミックなデータベース接続ハンドル
pub struct Connection {
    store: Arc<MVStore>,
    engine: SQLEngine,
}

impl Connection {
    /// ファイルベースのデータベースを開く（存在しない場合は自動生成）
    pub fn open<P: AsRef<Path>>(path: P) -> H2Result<Self> {
        let store = Arc::new(MVStore::open(path)?);
        let engine = SQLEngine::new(Arc::clone(&store))?;
        Ok(Self { store, engine })
    }

    /// インメモリデータベースを開く
    pub fn open_in_memory() -> H2Result<Self> {
        let store = Arc::new(MVStore::open_in_memory());
        let engine = SQLEngine::new(Arc::clone(&store))?;
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

    /// 基礎となるストレージエンジンの現在のバージョン番号を取得
    pub fn version(&self) -> u64 {
        self.store.current_version()
    }
}

pub type Result<T> = H2Result<T>;

#[cfg(test)]
mod tests {
    use super::*;
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
}
