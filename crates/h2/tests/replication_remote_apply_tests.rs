use std::time::Duration;

use h2::replication::{Instance, InstanceConfig, InstanceRole, SyncReplicationMode};
use h2::{H2Error, Value};

#[test]
fn test_replication_remote_apply_basic_crud() {
    // 1. Primary (Read-Write) 起動 (ポート 0 で OS 自動割当)
    let p_config = InstanceConfig {
        role: InstanceRole::Primary {
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            sync_mode: SyncReplicationMode::RemoteApply,
            apply_timeout: Duration::from_secs(5),
        },
    };
    let primary = Instance::open_in_memory(p_config).unwrap();
    let p_addr = primary.replication_addr().unwrap();
    assert!(!primary.is_read_only());

    // 2. Standby (Read-Only) 起動 (Primary へ接続)
    let s_config = InstanceConfig::standby(p_addr);
    let standby = Instance::open_in_memory(s_config).unwrap();
    assert!(standby.is_read_only());

    let p_conn = primary.connect().unwrap();
    let s_conn = standby.connect().unwrap();

    // 3. Primary で DDL 実行 (CREATE TABLE)
    p_conn
        .execute("CREATE TABLE products (id INT PRIMARY KEY, name VARCHAR, price DOUBLE);")
        .unwrap();

    // 4. Primary で INSERT
    p_conn
        .execute("INSERT INTO products VALUES (1, 'Laptop', 1299.99);")
        .unwrap();

    // remote_apply なので、INSERT の COMMIT が返った瞬間に Standby で可視！
    let rows = s_conn.query("SELECT id, name, price FROM products;").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get(0).unwrap(), &Value::Integer(1));
    assert_eq!(rows[0].get_as::<String>(1).unwrap(), "Laptop");

    // 5. Primary で UPDATE
    p_conn
        .execute("UPDATE products SET price = 1199.99 WHERE id = 1;")
        .unwrap();

    // Standby で即座に更新後価格が可視
    let rows = s_conn
        .query("SELECT price FROM products WHERE id = 1;")
        .unwrap();
    assert_eq!(rows[0].get(0).unwrap(), &Value::Double(1199.99));

    // 6. Primary で DELETE
    p_conn
        .execute("DELETE FROM products WHERE id = 1;")
        .unwrap();

    // Standby で即座に 0 行
    let rows = s_conn.query("SELECT * FROM products;").unwrap();
    assert_eq!(rows.len(), 0);
}

#[test]
fn test_replication_standby_read_only_enforcement() {
    let p_config = InstanceConfig {
        role: InstanceRole::Primary {
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            sync_mode: SyncReplicationMode::RemoteApply,
            apply_timeout: Duration::from_secs(5),
        },
    };
    let primary = Instance::open_in_memory(p_config).unwrap();
    let p_addr = primary.replication_addr().unwrap();

    let s_config = InstanceConfig::standby(p_addr);
    let standby = Instance::open_in_memory(s_config).unwrap();

    let p_conn = primary.connect().unwrap();
    let s_conn = standby.connect().unwrap();

    p_conn.execute("CREATE TABLE users (id INT, name VARCHAR);").unwrap();

    // Standby での書き込み系操作はすべて H2Error::ReadOnly で拒否される
    let res = s_conn.execute("INSERT INTO users VALUES (1, 'Alice');");
    assert!(matches!(res, Err(H2Error::ReadOnly(_))));

    let res = s_conn.execute("UPDATE users SET name = 'Bob';");
    assert!(matches!(res, Err(H2Error::ReadOnly(_))));

    let res = s_conn.execute("DELETE FROM users;");
    assert!(matches!(res, Err(H2Error::ReadOnly(_))));

    let res = s_conn.execute("CREATE TABLE test (id INT);");
    assert!(matches!(res, Err(H2Error::ReadOnly(_))));

    let res = s_conn.execute("DROP TABLE users;");
    assert!(matches!(res, Err(H2Error::ReadOnly(_))));

    let res = s_conn.execute("VACUUM;");
    assert!(matches!(res, Err(H2Error::ReadOnly(_))));

    // 読み取り系クエリ（SELECT, EXPLAIN）は Standby でも正常動作
    let rows = s_conn.query("SELECT * FROM users;").unwrap();
    assert_eq!(rows.len(), 0);

    let rows = s_conn.query("EXPLAIN SELECT * FROM users;").unwrap();
    assert!(!rows.is_empty());
}

#[test]
fn test_replication_explicit_transaction_remote_apply() {
    let p_config = InstanceConfig {
        role: InstanceRole::Primary {
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            sync_mode: SyncReplicationMode::RemoteApply,
            apply_timeout: Duration::from_secs(5),
        },
    };
    let primary = Instance::open_in_memory(p_config).unwrap();
    let p_addr = primary.replication_addr().unwrap();

    let s_config = InstanceConfig::standby(p_addr);
    let standby = Instance::open_in_memory(s_config).unwrap();

    let p_conn = primary.connect().unwrap();
    let s_conn = standby.connect().unwrap();

    p_conn
        .execute("CREATE TABLE ledger (id INT, amount INT);")
        .unwrap();

    // 明示的トランザクション
    p_conn.execute("BEGIN;").unwrap();
    p_conn.execute("INSERT INTO ledger VALUES (1, 500);").unwrap();
    p_conn.execute("INSERT INTO ledger VALUES (2, 300);").unwrap();
    p_conn.execute("INSERT INTO ledger VALUES (3, 200);").unwrap();

    // COMMIT 実行
    p_conn.execute("COMMIT;").unwrap();

    // Standby 側ですべてのレコードがコミット済みとして一度に反映されている
    let rows = s_conn.query("SELECT SUM(amount) FROM ledger;").unwrap();
    assert_eq!(rows[0].get(0).unwrap(), &Value::BigInt(1000));
}

#[test]
fn test_replication_initial_snapshot_sync() {
    // 1. Primary を起動し、あらかじめテーブル作成とデータ挿入を行う
    let p_config = InstanceConfig {
        role: InstanceRole::Primary {
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            sync_mode: SyncReplicationMode::RemoteApply,
            apply_timeout: Duration::from_secs(5),
        },
    };
    let primary = Instance::open_in_memory(p_config).unwrap();
    let p_addr = primary.replication_addr().unwrap();

    let p_conn = primary.connect().unwrap();
    p_conn
        .execute("CREATE TABLE pre_existing (id INT, tag VARCHAR);")
        .unwrap();
    p_conn
        .execute("INSERT INTO pre_existing VALUES (10, 'alpha'), (20, 'beta');")
        .unwrap();

    // 2. その後から Standby を起動・接続 (初期スナップショット同期が自動実行される)
    let s_config = InstanceConfig::standby(p_addr);
    let standby = Instance::open_in_memory(s_config).unwrap();
    let s_conn = standby.connect().unwrap();

    // 初期スナップショットにより、既存データが即座に Standby に存在
    let rows = s_conn
        .query("SELECT id, tag FROM pre_existing ORDER BY id;")
        .unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].get_as::<String>(1).unwrap(), "alpha");
    assert_eq!(rows[1].get_as::<String>(1).unwrap(), "beta");

    // 3. さらに Primary で新規データ挿入
    p_conn
        .execute("INSERT INTO pre_existing VALUES (30, 'gamma');")
        .unwrap();

    // リアルタイム同期も正常動作
    let rows = s_conn
        .query("SELECT tag FROM pre_existing WHERE id = 30;")
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get_as::<String>(0).unwrap(), "gamma");
}

#[test]
fn test_replication_ddl_and_sequence() {
    let p_config = InstanceConfig {
        role: InstanceRole::Primary {
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            sync_mode: SyncReplicationMode::RemoteApply,
            apply_timeout: Duration::from_secs(5),
        },
    };
    let primary = Instance::open_in_memory(p_config).unwrap();
    let p_addr = primary.replication_addr().unwrap();

    let s_config = InstanceConfig::standby(p_addr);
    let standby = Instance::open_in_memory(s_config).unwrap();

    let p_conn = primary.connect().unwrap();
    let s_conn = standby.connect().unwrap();

    // SERIAL 列（シーケンス自動生成）を含むテーブル
    p_conn
        .execute("CREATE TABLE orders (order_id SERIAL PRIMARY KEY, customer VARCHAR);")
        .unwrap();

    p_conn
        .execute("INSERT INTO orders (customer) VALUES ('Customer A'), ('Customer B');")
        .unwrap();

    // Standby でシーケンス採番値を確認
    let rows = s_conn
        .query("SELECT order_id, customer FROM orders ORDER BY order_id;")
        .unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].get(0).unwrap(), &Value::Integer(1));
    assert_eq!(rows[0].get_as::<String>(1).unwrap(), "Customer A");
    assert_eq!(rows[1].get(0).unwrap(), &Value::Integer(2));
    assert_eq!(rows[1].get_as::<String>(1).unwrap(), "Customer B");
}
