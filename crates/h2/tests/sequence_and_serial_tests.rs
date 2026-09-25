use std::sync::Arc;
use h2_mvstore::MVStore;
use h2_sql::executor::{ExecutionResult, SQLEngine};
use h2_types::Value;

#[test]
fn test_sequence_basic() {
    let store = Arc::new(MVStore::open_in_memory());
    let engine = SQLEngine::new(store).unwrap();

    engine.execute("CREATE SEQUENCE test_seq INCREMENT BY 5 START WITH 10;").unwrap();

    // NEXTVAL
    let res = engine.execute("SELECT NEXTVAL('test_seq');").unwrap();
    if let ExecutionResult::Query { rows, .. } = res {
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get(0).unwrap(), &Value::BigInt(10));
    } else {
        panic!("Expected query result");
    }

    // CURRVAL
    let res = engine.execute("SELECT CURRVAL('test_seq');").unwrap();
    if let ExecutionResult::Query { rows, .. } = res {
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get(0).unwrap(), &Value::BigInt(10));
    } else {
        panic!("Expected query result");
    }

    // NEXTVAL again
    let res = engine.execute("SELECT NEXTVAL('test_seq');").unwrap();
    if let ExecutionResult::Query { rows, .. } = res {
        assert_eq!(rows[0].get(0).unwrap(), &Value::BigInt(15));
    } else {
        panic!("Expected query result");
    }

    // SETVAL
    let res = engine.execute("SELECT SETVAL('test_seq', 100);").unwrap();
    if let ExecutionResult::Query { rows, .. } = res {
        assert_eq!(rows[0].get(0).unwrap(), &Value::BigInt(100));
    } else {
        panic!("Expected query result");
    }

    let res = engine.execute("SELECT NEXTVAL('test_seq');").unwrap();
    if let ExecutionResult::Query { rows, .. } = res {
        assert_eq!(rows[0].get(0).unwrap(), &Value::BigInt(105));
    } else {
        panic!("Expected query result");
    }

    // ALTER SEQUENCE RESTART WITH
    engine.execute("ALTER SEQUENCE test_seq RESTART WITH 1;").unwrap();
    let res = engine.execute("SELECT NEXTVAL('test_seq');").unwrap();
    if let ExecutionResult::Query { rows, .. } = res {
        assert_eq!(rows[0].get(0).unwrap(), &Value::BigInt(1));
    } else {
        panic!("Expected query result");
    }

    // DROP SEQUENCE
    engine.execute("DROP SEQUENCE test_seq;").unwrap();
    let err = engine.execute("SELECT NEXTVAL('test_seq');");
    assert!(err.is_err());
}

#[test]
fn test_serial_and_bigserial() {
    let store = Arc::new(MVStore::open_in_memory());
    let engine = SQLEngine::new(store).unwrap();

    engine.execute("CREATE TABLE users (id SERIAL PRIMARY KEY, name VARCHAR(50));").unwrap();

    engine.execute("INSERT INTO users (name) VALUES ('Alice');").unwrap();
    engine.execute("INSERT INTO users (name) VALUES ('Bob');").unwrap();
    engine.execute("INSERT INTO users (id, name) VALUES (DEFAULT, 'Charlie');").unwrap();

    let res = engine.execute("SELECT id, name FROM users ORDER BY id;").unwrap();
    if let ExecutionResult::Query { rows, .. } = res {
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].get(0).unwrap(), &Value::Integer(1));
        assert_eq!(rows[0].get(1).unwrap(), &Value::String("Alice".to_string()));
        assert_eq!(rows[1].get(0).unwrap(), &Value::Integer(2));
        assert_eq!(rows[1].get(1).unwrap(), &Value::String("Bob".to_string()));
        assert_eq!(rows[2].get(0).unwrap(), &Value::Integer(3));
        assert_eq!(rows[2].get(1).unwrap(), &Value::String("Charlie".to_string()));
    } else {
        panic!("Expected query result");
    }

    // BIGSERIAL
    engine.execute("CREATE TABLE big_items (id BIGSERIAL PRIMARY KEY, item VARCHAR(50));").unwrap();
    engine.execute("INSERT INTO big_items (item) VALUES ('item1');").unwrap();
    engine.execute("INSERT INTO big_items (item) VALUES ('item2');").unwrap();

    let res = engine.execute("SELECT id, item FROM big_items ORDER BY id;").unwrap();
    if let ExecutionResult::Query { rows, .. } = res {
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].get(0).unwrap(), &Value::BigInt(1));
        assert_eq!(rows[1].get(0).unwrap(), &Value::BigInt(2));
    } else {
        panic!("Expected query result");
    }
}

#[test]
fn test_identity_always() {
    let store = Arc::new(MVStore::open_in_memory());
    let engine = SQLEngine::new(store).unwrap();

    engine.execute("CREATE TABLE id_tbl (id INT GENERATED ALWAYS AS IDENTITY PRIMARY KEY, label VARCHAR(50));").unwrap();

    engine.execute("INSERT INTO id_tbl (label) VALUES ('first');").unwrap();
    engine.execute("INSERT INTO id_tbl (label) VALUES ('second');").unwrap();

    let res = engine.execute("SELECT id, label FROM id_tbl ORDER BY id;").unwrap();
    if let ExecutionResult::Query { rows, .. } = res {
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].get(0).unwrap(), &Value::Integer(1));
        assert_eq!(rows[1].get(0).unwrap(), &Value::Integer(2));
    } else {
        panic!("Expected query result");
    }
}

#[test]
fn test_oracle_style_sequence_syntax() {
    let store = Arc::new(MVStore::open_in_memory());
    let engine = SQLEngine::new(store).unwrap();

    engine.execute("CREATE SEQUENCE num_seq;").unwrap();

    let res = engine.execute("SELECT num_seq.NEXTVAL;").unwrap();
    if let ExecutionResult::Query { rows, .. } = res {
        assert_eq!(rows[0].get(0).unwrap(), &Value::BigInt(1));
    } else {
        panic!("Expected query result");
    }

    let res = engine.execute("SELECT num_seq.CURRVAL;").unwrap();
    if let ExecutionResult::Query { rows, .. } = res {
        assert_eq!(rows[0].get(0).unwrap(), &Value::BigInt(1));
    } else {
        panic!("Expected query result");
    }

    let res = engine.execute("SELECT num_seq.NEXTVAL;").unwrap();
    if let ExecutionResult::Query { rows, .. } = res {
        assert_eq!(rows[0].get(0).unwrap(), &Value::BigInt(2));
    } else {
        panic!("Expected query result");
    }
}
