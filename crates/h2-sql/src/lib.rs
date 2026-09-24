pub mod catalog;
pub mod executor;
pub mod expression;
pub mod parser;
pub mod row;

pub use catalog::{Catalog, ColumnDef, TableDef};
pub use executor::{ExecutionResult, SQLEngine};
pub use row::Row;

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
}
