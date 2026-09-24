use h2::Connection;

#[test]
fn test_create_and_drop_schema_basic() {
    let conn = Connection::open_in_memory().unwrap();

    // デフォルトで public スキーマが存在
    let rows = conn.query("SELECT schema_name FROM information_schema.schemata").unwrap();
    assert!(rows.iter().any(|r| r.get_as::<String>(0).unwrap() == "public"));

    // スキーマ作成
    conn.execute("CREATE SCHEMA tenant_a").unwrap();
    let rows = conn.query("SELECT schema_name FROM information_schema.schemata").unwrap();
    assert!(rows.iter().any(|r| r.get_as::<String>(0).unwrap() == "tenant_a"));

    // 重複エラー
    let err = conn.execute("CREATE SCHEMA tenant_a").unwrap_err();
    assert!(err.to_string().contains("already exists"), "error was: {}", err);

    // IF NOT EXISTS
    conn.execute("CREATE SCHEMA IF NOT EXISTS tenant_a").unwrap();

    // 存在しないスキーマ削除（IF EXISTS）
    conn.execute("DROP SCHEMA IF EXISTS non_existent").unwrap();

    // public スキーマの削除保護
    let err = conn.execute("DROP SCHEMA public").unwrap_err();
    assert!(err.to_string().contains("Cannot drop public schema"), "error was: {}", err);

    // スキーマ削除（空スキーマ）
    conn.execute("DROP SCHEMA tenant_a").unwrap();
    let rows = conn.query("SELECT schema_name FROM information_schema.schemata").unwrap();
    assert!(!rows.iter().any(|r| r.get_as::<String>(0).unwrap() == "tenant_a"));
}

#[test]
fn test_schema_qualified_tables_and_isolation() {
    let conn = Connection::open_in_memory().unwrap();

    conn.execute("CREATE SCHEMA tenant_a").unwrap();
    conn.execute("CREATE SCHEMA tenant_b").unwrap();

    // それぞれのスキーマに同名テーブルを作成
    conn.execute("CREATE TABLE tenant_a.users (id INT PRIMARY KEY, name VARCHAR(50))").unwrap();
    conn.execute("CREATE TABLE tenant_b.users (id INT PRIMARY KEY, name VARCHAR(50))").unwrap();
    conn.execute("CREATE TABLE users (id INT PRIMARY KEY, name VARCHAR(50))").unwrap(); // public

    conn.execute("INSERT INTO tenant_a.users VALUES (1, 'Alice Tenant A')").unwrap();
    conn.execute("INSERT INTO tenant_b.users VALUES (1, 'Bob Tenant B')").unwrap();
    conn.execute("INSERT INTO users VALUES (1, 'Public Admin')").unwrap();

    // データが完全に分離されていることを検証
    let rows_a = conn.query("SELECT name FROM tenant_a.users WHERE id = 1").unwrap();
    assert_eq!(rows_a[0].get_as::<String>(0).unwrap(), "Alice Tenant A");

    let rows_b = conn.query("SELECT name FROM tenant_b.users WHERE id = 1").unwrap();
    assert_eq!(rows_b[0].get_as::<String>(0).unwrap(), "Bob Tenant B");

    let rows_pub = conn.query("SELECT name FROM users WHERE id = 1").unwrap();
    assert_eq!(rows_pub[0].get_as::<String>(0).unwrap(), "Public Admin");

    // public.users でも引けること
    let rows_pub2 = conn.query("SELECT name FROM public.users WHERE id = 1").unwrap();
    assert_eq!(rows_pub2[0].get_as::<String>(0).unwrap(), "Public Admin");

    // DROP SCHEMA RESTRICT のエラー検証
    let err = conn.execute("DROP SCHEMA tenant_a").unwrap_err();
    assert!(err.to_string().contains("CASCADE"), "error was: {}", err);

    // DROP SCHEMA CASCADE
    conn.execute("DROP SCHEMA tenant_a CASCADE").unwrap();

    // 削除後のアクセス拒絶
    let err = conn.query("SELECT * FROM tenant_a.users").unwrap_err();
    assert!(err.to_string().contains("not found"), "error was: {}", err);

    // 他のスキーマのテーブルは無傷
    let rows_b = conn.query("SELECT name FROM tenant_b.users").unwrap();
    assert_eq!(rows_b.len(), 1);
}
