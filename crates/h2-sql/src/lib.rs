pub mod auth;
pub mod catalog;
pub mod executor;
pub mod expression;
pub mod fts;
pub mod memory;
pub mod parser;
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
