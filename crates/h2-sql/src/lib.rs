pub mod auth;
pub mod autovacuum;
pub mod catalog;
pub mod executor;
pub mod expression;
pub mod fts;
pub mod memory;
pub mod parser;
pub mod procedural;
pub mod query_stats;
pub mod row;
pub mod stats;
pub mod vectorized;
pub mod iceberg;

pub use auth::{AuthManager, Privilege, UserInfo};
pub use autovacuum::{AutoVacuumConfig, AutoVacuumCoordinator, TableActivity};
pub use catalog::{Catalog, ColumnDef, ColumnStats, IndexDef, TableDef, TableStats, VirtualGraphKind, parse_virtual_graph_table};
pub use executor::{ExecutionResult, SQLEngine};
pub use parser::{convert_data_type, parse_sql, parse_sql_mode};
pub use procedural::ProceduralEngine;
pub use fts::{FtsIndex, MorphTokenizer, NGramTokenizer, Tokenizer, TokenizerKind};
pub use memory::{ExternalSorter, MemoryConfig, MemoryGrant, MemoryGrantCoordinator, MemoryTracker};
pub use row::Row;
pub use stats::{analyze_table, analyze_table_with_sample, estimate_scan_cost, estimate_selectivity, SampleSpec};
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

    #[test]
    fn test_sql_virtual_graph_tables() {
        let store = Arc::new(MVStore::open_in_memory());
        let graph_engine = h2_graph::GraphEngine::new(store.clone(), "social").unwrap();

        // 1. Populate graph via Cypher
        graph_engine.execute("CREATE (a:Person {name: 'Alice', age: 30})").unwrap();
        graph_engine.execute("CREATE (b:Person {name: 'Bob', age: 25})").unwrap();
        graph_engine.execute("MATCH (a:Person {name: 'Alice'}), (b:Person {name: 'Bob'}) CREATE (a)-[:KNOWS {since: 2021}]->(b)").unwrap();

        // 2. Query virtual graph nodes table from SQL
        let sql_engine = SQLEngine::new(store.clone()).unwrap();
        let res_nodes = sql_engine.execute("SELECT id, labels, properties FROM graph_social_nodes").unwrap();
        if let ExecutionResult::Query { columns, rows } = res_nodes {
            assert_eq!(columns, vec!["id", "labels", "properties"]);
            assert_eq!(rows.len(), 2);
        } else {
            panic!("Expected Query result for graph_social_nodes");
        }

        // 3. Query virtual graph edges table from SQL
        let res_edges = sql_engine.execute("SELECT id, src_id, dst_id, type, properties FROM graph_social_edges").unwrap();
        if let ExecutionResult::Query { columns, rows } = res_edges {
            assert_eq!(columns, vec!["id", "src_id", "dst_id", "type", "properties"]);
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].get(3), Some(&Value::String("KNOWS".to_string())));
        } else {
            panic!("Expected Query result for graph_social_edges");
        }

        // 4. SQL JOIN between virtual nodes and edges
        let join_sql = "SELECT e.type, src.id AS src_node, dst.id AS dst_node \
                        FROM graph_social_edges e \
                        JOIN graph_social_nodes src ON e.src_id = src.id \
                        JOIN graph_social_nodes dst ON e.dst_id = dst.id";
        let res_join = sql_engine.execute(join_sql).unwrap();
        if let ExecutionResult::Query { rows, .. } = res_join {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].get(0), Some(&Value::String("KNOWS".to_string())));
        } else {
            panic!("Expected Query result for virtual graph JOIN");
        }
    }

    #[test]
    fn test_sql_cypher_table_function() {
        let store = Arc::new(MVStore::open_in_memory());
        let graph_engine = h2_graph::GraphEngine::new(store.clone(), "company").unwrap();

        graph_engine.execute("CREATE (:Employee {name: 'Carol', salary: 120000})").unwrap();
        graph_engine.execute("CREATE (:Employee {name: 'Dave', salary: 90000})").unwrap();

        let sql_engine = SQLEngine::new(store).unwrap();

        // 1. Basic CYPHER() TVF in FROM clause
        let sql = "SELECT * FROM cypher('company', 'MATCH (e:Employee) RETURN e.name AS name, e.salary AS salary ORDER BY salary DESC') AS g";
        let res = sql_engine.execute(sql).unwrap();
        if let ExecutionResult::Query { columns, rows } = res {
            assert_eq!(columns, vec!["name", "salary"]);
            assert_eq!(rows.len(), 2);
            assert_eq!(rows[0].get(0), Some(&Value::String("Carol".to_string())));
            assert_eq!(rows[1].get(0), Some(&Value::String("Dave".to_string())));
        } else {
            panic!("Expected Query result for cypher() TVF");
        }

        // 2. CYPHER() TVF with explicit column aliases and SQL WHERE
        let sql_filter = "SELECT g.emp_name, g.emp_salary FROM cypher('company', 'MATCH (e:Employee) RETURN e.name, e.salary') AS g(emp_name, emp_salary) WHERE CAST(g.emp_salary AS BIGINT) > 100000";
        let res_filter = sql_engine.execute(sql_filter).unwrap();
        if let ExecutionResult::Query { columns, rows } = res_filter {
            assert_eq!(columns, vec!["g.emp_name", "g.emp_salary"]);
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].get(0), Some(&Value::String("Carol".to_string())));
        } else {
            panic!("Expected Query result for filtered cypher() TVF");
        }
    }

    #[test]
    fn test_sql_relational_and_graph_join() {
        let store = Arc::new(MVStore::open_in_memory());
        let sql_engine = SQLEngine::new(store.clone()).unwrap();
        let graph_engine = h2_graph::GraphEngine::new(store, "knowledge").unwrap();

        // Relational table
        sql_engine.execute("CREATE TABLE users (id INT PRIMARY KEY, email VARCHAR)").unwrap();
        sql_engine.execute("INSERT INTO users VALUES (1, 'alice@acme.com'), (2, 'bob@acme.com')").unwrap();

        // Graph nodes
        graph_engine.execute("CREATE (:Person {user_id: 1, role: 'Architect'})").unwrap();
        graph_engine.execute("CREATE (:Person {user_id: 2, role: 'Engineer'})").unwrap();

        // Hybrid query: SQL relational table JOIN with Cypher TVF
        let hybrid_sql = "SELECT u.id, u.email, g.role \
                          FROM users u \
                          JOIN cypher('knowledge', 'MATCH (p:Person) RETURN p.user_id AS uid, p.role AS role') AS g(uid, role) \
                          ON u.id = CAST(g.uid AS INT) \
                          WHERE u.id = 1";

        let res = sql_engine.execute(hybrid_sql).unwrap();
        if let ExecutionResult::Query { columns, rows } = res {
            assert_eq!(columns, vec!["u.id", "u.email", "g.role"]);
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].get(0), Some(&Value::Integer(1)));
            assert_eq!(rows[0].get(1), Some(&Value::String("alice@acme.com".to_string())));
            assert_eq!(rows[0].get(2), Some(&Value::String("Architect".to_string())));
        } else {
            panic!("Expected Query result for hybrid relational-graph JOIN");
        }
    }

    #[test]
    fn test_sampling_analyze_and_show_stats() {
        let store = Arc::new(MVStore::open_in_memory());
        let engine = SQLEngine::new(store).unwrap();

        engine.execute("CREATE TABLE sample_test (id INT PRIMARY KEY, category VARCHAR, val INT)").unwrap();

        // 100 行挿入（category: 'A' 60回, 'B' 30回, 'C' 10回）
        for i in 1..=100 {
            let cat = if i <= 60 { "A" } else if i <= 90 { "B" } else { "C" };
            engine.execute(&format!("INSERT INTO sample_test VALUES ({}, '{}', {})", i, cat, i * 10)).unwrap();
        }

        // 1. SAMPLE ROWS による ANALYZE
        engine.execute("ANALYZE sample_test WITH SAMPLE 40 ROWS").unwrap();

        let stats_res = engine.execute("SHOW STATS FOR sample_test").unwrap();
        if let ExecutionResult::Query { columns, rows } = stats_res {
            assert_eq!(columns[0], "COLUMN_NAME");
            // category 列の統計を検証
            let cat_row = rows.iter().find(|r| r.values[0] == Value::String("category".to_string())).unwrap();
            let ndv = match cat_row.values[2] {
                Value::BigInt(n) => n,
                _ => panic!("Expected BigInt NDV"),
            };
            assert!(ndv >= 2 && ndv <= 3);
            let mcv_str = match &cat_row.values[5] {
                Value::String(s) => s.clone(),
                _ => panic!("Expected String MCV"),
            };
            assert!(mcv_str.contains("\"A\""));
        } else {
            panic!("Expected SHOW STATS Query result");
        }

        // テーブル全体の SHOW STATS で sample_ratio が 0.4 になっていることを検証
        let all_stats = engine.execute("SHOW STATS").unwrap();
        if let ExecutionResult::Query { rows, .. } = all_stats {
            let t_row = rows.iter().find(|r| r.values[0] == Value::String("sample_test".to_string())).unwrap();
            let sample_ratio = match t_row.values[3] {
                Value::Double(d) => d,
                _ => panic!("Expected Double sample_ratio"),
            };
            assert!((sample_ratio - 0.4).abs() < 0.05);
        } else {
            panic!("Expected SHOW STATS Query result");
        }

        // 2. SAMPLE PERCENT による ANALYZE
        engine.execute("ANALYZE sample_test WITH SAMPLE 50 PERCENT").unwrap();
        let all_stats50 = engine.execute("SHOW STATS").unwrap();
        if let ExecutionResult::Query { rows, .. } = all_stats50 {
            let t_row = rows.iter().find(|r| r.values[0] == Value::String("sample_test".to_string())).unwrap();
            let sample_ratio = match t_row.values[3] {
                Value::Double(d) => d,
                _ => panic!("Expected Double sample_ratio"),
            };
            assert!((sample_ratio - 0.5).abs() < 0.05);
        } else {
            panic!("Expected SHOW STATS Query result");
        }
    }

    #[test]
    fn test_autovacuum_thresholds_and_dead_tuple_pruning() {
        let store = Arc::new(MVStore::open_in_memory());
        let engine = SQLEngine::new(store).unwrap();

        // 閾値を小さく設定
        engine.autovacuum().set_config(AutoVacuumConfig {
            vacuum_threshold: 4,
            vacuum_scale_factor: 0.0,
            analyze_threshold: 8,
            analyze_scale_factor: 0.0,
            iceberg_compaction_file_threshold: 10,
            enabled: true,
        });

        engine.execute("CREATE TABLE auto_test (id INT PRIMARY KEY, val INT)").unwrap();

        // 10 行一括挿入 -> mod_count = 10 (閾値 8 超え) -> 自動 ANALYZE が発火
        let values_str = (1..=10).map(|i| format!("({}, {})", i, i)).collect::<Vec<_>>().join(", ");
        engine.execute(&format!("INSERT INTO auto_test VALUES {}", values_str)).unwrap();

        let act = engine.autovacuum().get_or_create_activity("auto_test");
        assert_eq!(act.mod_count.load(std::sync::atomic::Ordering::SeqCst), 0); // 自動ANALYZEによりリセットされた
        assert!(act.last_analyze_time.load(std::sync::atomic::Ordering::SeqCst) > 0);

        // 5 行一括更新 -> dead_tuple_count = 5 (閾値 4 超え) -> 自動 VACUUM が発火
        engine.execute("UPDATE auto_test SET val = val + 100 WHERE id <= 5").unwrap();

        assert_eq!(act.dead_tuple_count.load(std::sync::atomic::Ordering::SeqCst), 0); // 自動VACUUMによりリセットされた
        assert!(act.last_vacuum_time.load(std::sync::atomic::Ordering::SeqCst) > 0);

        // データ整合性の確認
        let res = engine.execute("SELECT val FROM auto_test WHERE id = 1").unwrap();
        if let ExecutionResult::Query { rows, .. } = res {
            assert_eq!(rows[0].values[0], Value::Integer(101));
        } else {
            panic!("Expected Query result");
        }
    }

    #[test]
    fn test_vacuum_vs_vacuum_full_statements() {
        let store = Arc::new(MVStore::open_in_memory());
        let engine = SQLEngine::new(store).unwrap();

        engine.execute("CREATE TABLE vac_test (id INT PRIMARY KEY, info VARCHAR)").unwrap();
        engine.execute("INSERT INTO vac_test VALUES (1, 'old'), (2, 'stay')").unwrap();
        engine.execute("DELETE FROM vac_test WHERE id = 1").unwrap();

        // 通常 VACUUM（オンラインデッドタプル回収）
        let vac_res = engine.execute("VACUUM vac_test").unwrap();
        assert!(matches!(vac_res, ExecutionResult::Ddl));

        // VACUUM FULL（ファイル再構築物理コンパクション）
        let full_res = engine.execute("VACUUM FULL").unwrap();
        assert!(matches!(full_res, ExecutionResult::Ddl));

        // 生存レコードが正しく参照できること
        let res = engine.execute("SELECT info FROM vac_test WHERE id = 2").unwrap();
        if let ExecutionResult::Query { rows, .. } = res {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::String("stay".to_string()));
        } else {
            panic!("Expected Query result");
        }
    }
}

