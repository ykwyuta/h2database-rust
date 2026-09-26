use std::sync::Arc;
use h2_mvstore::MVStore;
use h2_sql::executor::{ExecutionResult, SQLEngine};
use h2_types::Value;

#[test]
fn test_backup_and_restore() {
    let temp_dir = std::env::temp_dir();
    let backup_path = temp_dir.join("h2_test_backup.h2bk");
    let backup_file = backup_path.to_str().unwrap().replace('\\', "/");

    let store = Arc::new(MVStore::open_in_memory());
    let engine = SQLEngine::new(store).unwrap();

    engine.execute("CREATE TABLE products (id INT PRIMARY KEY, name VARCHAR(50), price INT);").unwrap();
    engine.execute("INSERT INTO products VALUES (1, 'Apple', 100), (2, 'Banana', 200);").unwrap();

    // BACKUP TO
    engine.execute(&format!("BACKUP TO '{}';", backup_file)).unwrap();

    // データを変更（削除）
    engine.execute("DELETE FROM products WHERE id = 1;").unwrap();
    let res = engine.execute("SELECT COUNT(*) FROM products;").unwrap();
    if let ExecutionResult::Query { rows, .. } = res {
        assert_eq!(rows[0].get(0).unwrap(), &Value::BigInt(1));
    }

    // RESTORE FROM
    engine.execute(&format!("RESTORE FROM '{}';", backup_file)).unwrap();

    // 復元確認
    let res = engine.execute("SELECT id, name, price FROM products ORDER BY id;").unwrap();
    if let ExecutionResult::Query { rows, .. } = res {
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].get(1).unwrap(), &Value::String("Apple".to_string()));
        assert_eq!(rows[1].get(1).unwrap(), &Value::String("Banana".to_string()));
    } else {
        panic!("Expected query result");
    }

    let _ = std::fs::remove_file(backup_path);
}

#[test]
fn test_script_and_runscript() {
    let temp_dir = std::env::temp_dir();
    let script_path = temp_dir.join("h2_test_script.sql");
    let script_file = script_path.to_str().unwrap().replace('\\', "/");

    let store1 = Arc::new(MVStore::open_in_memory());
    let engine1 = SQLEngine::new(store1).unwrap();

    engine1.execute("CREATE SEQUENCE num_seq INCREMENT BY 2 START WITH 10;").unwrap();
    engine1.execute("CREATE TABLE employees (id INT PRIMARY KEY, name VARCHAR(50), salary INT);").unwrap();
    engine1.execute("INSERT INTO employees VALUES (1, 'Alice', 5000), (2, 'Bob', 6000);").unwrap();

    // SCRIPT TO
    engine1.execute(&format!("SCRIPT TO '{}';", script_file)).unwrap();

    // 新規エンジンで RUNSCRIPT FROM
    let store2 = Arc::new(MVStore::open_in_memory());
    let engine2 = SQLEngine::new(store2).unwrap();

    engine2.execute(&format!("RUNSCRIPT FROM '{}';", script_file)).unwrap();

    let res = engine2.execute("SELECT id, name, salary FROM employees ORDER BY id;").unwrap();
    if let ExecutionResult::Query { rows, .. } = res {
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].get(1).unwrap(), &Value::String("Alice".to_string()));
        assert_eq!(rows[1].get(1).unwrap(), &Value::String("Bob".to_string()));
    } else {
        panic!("Expected query result");
    }

    let _ = std::fs::remove_file(script_path);
}

#[test]
fn test_copy_to_and_from_csv() {
    let temp_dir = std::env::temp_dir();
    let csv_path = temp_dir.join("h2_test_users.csv");
    let csv_file = csv_path.to_str().unwrap().replace('\\', "/");

    let store = Arc::new(MVStore::open_in_memory());
    let engine = SQLEngine::new(store).unwrap();

    engine.execute("CREATE TABLE users (id INT PRIMARY KEY, name VARCHAR(50), city VARCHAR(50));").unwrap();
    engine.execute("INSERT INTO users VALUES (1, 'Alice', 'Tokyo'), (2, 'Bob', 'Osaka, Japan'), (3, 'Charlie', 'New York');").unwrap();

    // COPY TO
    engine.execute(&format!("COPY users TO '{}' WITH (FORMAT CSV, HEADER);", csv_file)).unwrap();

    let content = std::fs::read_to_string(&csv_path).unwrap();
    assert!(content.contains("id,name,city"));
    assert!(content.contains("\"Osaka, Japan\""));

    // 新規テーブルへ COPY FROM
    engine.execute("CREATE TABLE users_copy (id INT PRIMARY KEY, name VARCHAR(50), city VARCHAR(50));").unwrap();
    engine.execute(&format!("COPY users_copy FROM '{}' WITH (FORMAT CSV, HEADER);", csv_file)).unwrap();

    let res = engine.execute("SELECT id, name, city FROM users_copy ORDER BY id;").unwrap();
    if let ExecutionResult::Query { rows, .. } = res {
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].get(1).unwrap(), &Value::String("Alice".to_string()));
        assert_eq!(rows[1].get(2).unwrap(), &Value::String("Osaka, Japan".to_string()));
        assert_eq!(rows[2].get(1).unwrap(), &Value::String("Charlie".to_string()));
    } else {
        panic!("Expected query result");
    }

    let _ = std::fs::remove_file(csv_path);
}

#[test]
fn test_cursors_declare_fetch_close() {
    let store = Arc::new(MVStore::open_in_memory());
    let engine = SQLEngine::new(store).unwrap();

    engine.execute("CREATE TABLE items (id INT PRIMARY KEY, val VARCHAR(10));").unwrap();
    engine.execute("INSERT INTO items VALUES (1, 'A'), (2, 'B'), (3, 'C'), (4, 'D'), (5, 'E');").unwrap();

    // DECLARE
    engine.execute("DECLARE cur SCROLL CURSOR FOR SELECT id, val FROM items ORDER BY id;").unwrap();

    // FETCH NEXT -> 1
    let res = engine.execute("FETCH NEXT FROM cur;").unwrap();
    if let ExecutionResult::Query { rows, .. } = res {
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get(0).unwrap(), &Value::Integer(1));
    }

    // FETCH NEXT -> 2
    let res = engine.execute("FETCH NEXT FROM cur;").unwrap();
    if let ExecutionResult::Query { rows, .. } = res {
        assert_eq!(rows[0].get(0).unwrap(), &Value::Integer(2));
    }

    // FETCH PRIOR -> 1
    let res = engine.execute("FETCH PRIOR FROM cur;").unwrap();
    if let ExecutionResult::Query { rows, .. } = res {
        assert_eq!(rows[0].get(0).unwrap(), &Value::Integer(1));
    }

    // FETCH LAST -> 5
    let res = engine.execute("FETCH LAST FROM cur;").unwrap();
    if let ExecutionResult::Query { rows, .. } = res {
        assert_eq!(rows[0].get(0).unwrap(), &Value::Integer(5));
    }

    // FETCH FIRST -> 1
    let res = engine.execute("FETCH FIRST FROM cur;").unwrap();
    if let ExecutionResult::Query { rows, .. } = res {
        assert_eq!(rows[0].get(0).unwrap(), &Value::Integer(1));
    }

    // FETCH ABSOLUTE 3 -> 3
    let res = engine.execute("FETCH ABSOLUTE 3 FROM cur;").unwrap();
    if let ExecutionResult::Query { rows, .. } = res {
        assert_eq!(rows[0].get(0).unwrap(), &Value::Integer(3));
    }

    // FETCH RELATIVE -1 -> 2
    let res = engine.execute("FETCH RELATIVE -1 FROM cur;").unwrap();
    if let ExecutionResult::Query { rows, .. } = res {
        assert_eq!(rows[0].get(0).unwrap(), &Value::Integer(2));
    }

    // FETCH ALL -> 3, 4, 5
    let res = engine.execute("FETCH ALL FROM cur;").unwrap();
    if let ExecutionResult::Query { rows, .. } = res {
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].get(0).unwrap(), &Value::Integer(3));
        assert_eq!(rows[1].get(0).unwrap(), &Value::Integer(4));
        assert_eq!(rows[2].get(0).unwrap(), &Value::Integer(5));
    }

    // CLOSE cur
    engine.execute("CLOSE cur;").unwrap();

    // FETCH after CLOSE -> Error
    let err = engine.execute("FETCH NEXT FROM cur;");
    assert!(err.is_err());
}
