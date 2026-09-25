pub mod auth;
pub mod catalog;
pub mod executor;
pub mod expression;
pub mod fts;
pub mod memory;
pub mod parser;
pub mod query_stats;
pub mod row;
pub mod stats;
pub mod vectorized;

pub use auth::{AuthManager, Privilege, UserInfo};
pub use catalog::{Catalog, ColumnDef, ColumnStats, IndexDef, TableDef, TableStats};
pub use executor::{ExecutionResult, SQLEngine};
pub use fts::{FtsIndex, MorphTokenizer, NGramTokenizer, Tokenizer, TokenizerKind};
pub use memory::{ExternalSorter, MemoryConfig, MemoryGrant, MemoryGrantCoordinator, MemoryTracker};
pub use row::Row;
pub use stats::{analyze_table, estimate_scan_cost, estimate_selectivity};
pub use vectorized::{
    create_arrow_schema, scan_slotted_page_to_batch, ExecutionMode, MemoryBatchOperator,
    MemoryRowOperator, PhysicalOperator, RowToVectorAdapter, VectorAggregateOp, VectorChunk,
    VectorToRowAdapter, VectorizedAggregate, VectorizedFilter,
};

#[cfg(test)]
mod tests {
    use super::*;
    use h2_mvstore::MVStore;
    use h2_types::Value;
    use std::sync::Arc;

    #[test]
    fn test_sql_engine_end_to_end() {
        let store = Arc::new(MVStore::open_in_memory());
        let engine = SQLEngine::new(store).unwrap();

        // 1. CREATE TABLE
        let ddl_res = engine.execute(
            "CREATE TABLE users (
                id INTEGER PRIMARY KEY,
                name VARCHAR,
                balance DECIMAL(10, 2)
            )",
        ).unwrap();
        assert!(matches!(ddl_res, ExecutionResult::Ddl));

        // 2. INSERT
        let insert_res = engine.execute(
            "INSERT INTO users VALUES (1, 'Alice', 150.50), (2, 'Bob', 80.00), (3, 'Charlie', 300.00)",
        ).unwrap();
        if let ExecutionResult::Dml { affected_rows } = insert_res {
            assert_eq!(affected_rows, 3);
        } else {
            panic!("Expected DML result");
        }

        // 3. SELECT *
        let query_res = engine.execute("SELECT * FROM users").unwrap();
        if let ExecutionResult::Query { columns, rows } = query_res {
            assert_eq!(columns, vec!["id", "name", "balance"]);
            assert_eq!(rows.len(), 3);
        } else {
            panic!("Expected Query result");
        }

        // 4. SELECT with WHERE filter
        let filtered_res = engine.execute("SELECT name, balance FROM users WHERE balance > 100").unwrap();
        if let ExecutionResult::Query { columns, rows } = filtered_res {
            assert_eq!(columns, vec!["name", "balance"]);
            assert_eq!(rows.len(), 2);
            assert_eq!(rows[0].get(0), Some(&Value::String("Alice".to_string())));
            assert_eq!(rows[1].get(0), Some(&Value::String("Charlie".to_string())));
        } else {
            panic!("Expected Query result");
        }
    }

    #[test]
    fn test_explain_analyze_and_query_stats() {
        let store = Arc::new(MVStore::open_in_memory());
        let engine = SQLEngine::new(store).unwrap();
        engine.execute("CREATE TABLE metrics_test (id INTEGER PRIMARY KEY, balance INTEGER)").unwrap();
        engine.execute("INSERT INTO metrics_test VALUES (1, 10), (2, 20)").unwrap();

        let plan = engine.execute("EXPLAIN UPDATE metrics_test SET balance = balance + 1 WHERE id = 1").unwrap();
        let ExecutionResult::Query { rows, .. } = plan else { panic!("expected plan") };
        let Value::String(plan_text) = &rows[0].values[0] else { panic!("expected text") };
        assert!(plan_text.contains("IndexScan"));

        let before = engine.execute("SELECT balance FROM metrics_test WHERE id = 1").unwrap();
        let ExecutionResult::Query { rows, .. } = before else { panic!("expected rows") };
        assert_eq!(rows[0].values[0], Value::Integer(10));

        let plan = engine.execute("EXPLAIN ANALYZE UPDATE metrics_test SET balance = balance + 1 WHERE id = 1").unwrap();
        let ExecutionResult::Query { rows, .. } = plan else { panic!("expected plan") };
        let Value::String(plan_text) = &rows[0].values[0] else { panic!("expected text") };
        assert!(plan_text.contains("Actual Rows: 1"));
        assert!(plan_text.contains("Waits:"));
        assert!(plan_text.contains("Storage: point_gets=1, scans=1, scan_entries=1"));

        engine.execute("UPDATE metrics_test SET balance = balance + 1 WHERE id = 1").unwrap();
        engine.execute("update metrics_test set balance=balance+1 where id=2").unwrap();
        let stats = engine.execute("SHOW QUERY STATS").unwrap();
        let ExecutionResult::Query { columns, rows } = stats else { panic!("expected stats") };
        assert!(columns.contains(&"wal_sync_ms".to_string()));
        assert!(columns.contains(&"wal_durable_wait_ms".to_string()));
        assert!(rows.iter().any(|r| r.values[0] == Value::String(
            crate::query_stats::normalize_query("UPDATE metrics_test SET balance=balance+1 WHERE id=1")
        ) && r.values[1] == Value::BigInt(2)));

        engine.execute("RESET QUERY STATS").unwrap();
        let stats = engine.execute("SHOW QUERY STATS").unwrap();
        let ExecutionResult::Query { rows, .. } = stats else { panic!("expected stats") };
        assert!(rows.is_empty());

        let tx = engine.tx_store().begin();
        engine.execute_with_tx(&tx, "UPDATE metrics_test SET balance = balance + 1 WHERE id = 1").unwrap();
        engine.commit_transaction(&tx).unwrap();
        let stats = engine.execute("SHOW QUERY STATS").unwrap();
        let ExecutionResult::Query { rows, .. } = stats else { panic!("expected stats") };
        assert!(rows.iter().any(|r| r.values[0] == Value::String("COMMIT".to_string())));
    }

    #[test]
    fn test_explain_analyze_respects_read_only_and_stats_permissions() {
        let store = Arc::new(MVStore::open_in_memory());
        let engine = SQLEngine::new(store).unwrap();
        engine.execute("CREATE TABLE restricted_test (id INTEGER PRIMARY KEY, amount INTEGER)").unwrap();
        engine.execute("INSERT INTO restricted_test VALUES (1, 5)").unwrap();
        engine.auth().create_user("reader", None, None, false).unwrap();
        assert!(matches!(
            engine.execute_with_user("SHOW QUERY STATS", Some("reader")),
            Err(h2_types::H2Error::PermissionDenied(_))
        ));
        assert!(matches!(
            engine.execute_with_user("EXPLAIN UPDATE restricted_test SET amount = 6 WHERE id = 1", Some("reader")),
            Err(h2_types::H2Error::PermissionDenied(_))
        ));

        engine.set_read_only(true);
        assert!(engine.execute("EXPLAIN UPDATE restricted_test SET amount = 6 WHERE id = 1").is_ok());
        assert!(matches!(
            engine.execute("EXPLAIN ANALYZE UPDATE restricted_test SET amount = 6 WHERE id = 1"),
            Err(h2_types::H2Error::ReadOnly(_))
        ));
        let result = engine.execute("SELECT amount FROM restricted_test WHERE id = 1").unwrap();
        let ExecutionResult::Query { rows, .. } = result else { panic!("expected rows") };
        assert_eq!(rows[0].values[0], Value::Integer(5));
    }

    #[test]
    fn test_sql_transaction_rollback_and_isolation() {
        let store = Arc::new(MVStore::open_in_memory());
        let engine = SQLEngine::new(store).unwrap();

        engine.execute("CREATE TABLE accounts (id INTEGER, owner VARCHAR, amount INTEGER)").unwrap();
        engine.execute("INSERT INTO accounts VALUES (1, 'Alice', 1000)").unwrap();

        // トランザクション開始
        let tx = engine.tx_store().begin();
        engine.execute_with_tx(&tx, "INSERT INTO accounts VALUES (2, 'Bob', 500)").unwrap();

        // 別トランザクション（暗黙）からは、未コミットのBobは見えない
        let res_before = engine.execute("SELECT * FROM accounts").unwrap();
        if let ExecutionResult::Query { rows, .. } = res_before {
            assert_eq!(rows.len(), 1);
        }

        // ロールバック
        tx.rollback().unwrap();

        // ロールバック後もAliceのみ
        let res_after = engine.execute("SELECT * FROM accounts").unwrap();
        if let ExecutionResult::Query { rows, .. } = res_after {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].get(1), Some(&Value::String("Alice".to_string())));
        }
    }

    #[test]
    fn test_fulltext_search_ngram_and_morph() {
        let store = Arc::new(MVStore::open_in_memory());
        let engine = SQLEngine::new(store).unwrap();

        engine.execute("CREATE TABLE articles (id INTEGER, title VARCHAR, content VARCHAR)").unwrap();
        engine.execute("INSERT INTO articles VALUES (1, 'Rust言語入門', 'Rustは高速でメモリ安全な言語です')").unwrap();
        engine.execute("INSERT INTO articles VALUES (2, 'H2 Database解説', 'H2は軽量な組み込みJavaリレーショナルデータベースです')").unwrap();
        engine.execute("INSERT INTO articles VALUES (3, 'データベースの歴史', 'リレーショナルモデルとデータベースとSQLの進化について')").unwrap();

        // 1. FT_SEARCH (N-Gram / Bigram) による日本語部分一致検索
        let res1 = engine.execute("SELECT id, title FROM articles WHERE FT_SEARCH(content, 'データベース')").unwrap();
        if let ExecutionResult::Query { rows, .. } = res1 {
            assert_eq!(rows.len(), 2);
            let titles: Vec<String> = rows.iter().map(|r| r.get(1).unwrap().to_string()).collect();
            assert!(titles.contains(&"'H2 Database解説'".to_string()));
            assert!(titles.contains(&"'データベースの歴史'".to_string()));
        } else {
            panic!("Expected Query result");
        }

        // 2. FT_SEARCH_MORPH (形態素解析) による単語検索
        let res2 = engine.execute("SELECT id, title FROM articles WHERE FT_SEARCH_MORPH(content, 'メモリ安全')").unwrap();
        if let ExecutionResult::Query { rows, .. } = res2 {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].get(1), Some(&Value::String("Rust言語入門".to_string())));
        } else {
            panic!("Expected Query result");
        }

        // 3. FT_SEARCH による単語部分一致検索
        let res3 = engine.execute("SELECT id, title FROM articles WHERE FT_SEARCH(content, '高速')").unwrap();
        if let ExecutionResult::Query { rows, .. } = res3 {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].get(1), Some(&Value::String("Rust言語入門".to_string())));
        } else {
            panic!("Expected Query result");
        }
    }
}
