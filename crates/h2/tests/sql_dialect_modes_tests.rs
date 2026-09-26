use h2::{Connection, SqlDialectMode};

#[test]
fn test_mode_switching_and_show_mode() {
    let conn = Connection::open_in_memory().unwrap();

    // 1. デフォルトは REGULAR
    assert_eq!(conn.get_mode(), SqlDialectMode::Regular);
    let rows = conn.query("SHOW MODE").unwrap();
    assert_eq!(rows[0].get_as::<String>(0).unwrap(), "REGULAR");

    let rows = conn.query("SELECT CURRENT_MODE()").unwrap();
    assert_eq!(rows[0].get_as::<String>(0).unwrap(), "REGULAR");

    // 2. SET MODE による動的切替 (各種エイリアス含む)
    conn.execute("SET MODE MySQL").unwrap();
    assert_eq!(conn.get_mode(), SqlDialectMode::MySql);
    let rows = conn.query("SHOW MODE").unwrap();
    assert_eq!(rows[0].get_as::<String>(0).unwrap(), "MYSQL");

    conn.execute("SET MODE MariaDB").unwrap();
    assert_eq!(conn.get_mode(), SqlDialectMode::MySql);

    conn.execute("SET MODE Oracle").unwrap();
    assert_eq!(conn.get_mode(), SqlDialectMode::Oracle);
    let rows = conn.query("SHOW MODE").unwrap();
    assert_eq!(rows[0].get_as::<String>(0).unwrap(), "ORACLE");

    conn.execute("SET MODE MSSQLServer").unwrap();
    assert_eq!(conn.get_mode(), SqlDialectMode::MsSqlServer);
    let rows = conn.query("SHOW MODE").unwrap();
    assert_eq!(rows[0].get_as::<String>(0).unwrap(), "MSSQLSERVER");

    conn.execute("SET MODE TSQL").unwrap();
    assert_eq!(conn.get_mode(), SqlDialectMode::MsSqlServer);

    conn.execute("SET MODE DB2").unwrap();
    assert_eq!(conn.get_mode(), SqlDialectMode::Db2);
    let rows = conn.query("SHOW MODE").unwrap();
    assert_eq!(rows[0].get_as::<String>(0).unwrap(), "DB2");

    conn.execute("SET MODE PostgreSQL").unwrap();
    assert_eq!(conn.get_mode(), SqlDialectMode::PostgreSql);
    let rows = conn.query("SHOW MODE").unwrap();
    assert_eq!(rows[0].get_as::<String>(0).unwrap(), "POSTGRESQL");

    conn.execute("SET MODE PG").unwrap();
    assert_eq!(conn.get_mode(), SqlDialectMode::PostgreSql);

    conn.execute("SET MODE Regular").unwrap();
    assert_eq!(conn.get_mode(), SqlDialectMode::Regular);
}

#[test]
fn test_oracle_compatibility_mode() {
    let conn = Connection::open_in_memory_with_mode(SqlDialectMode::Oracle).unwrap();

    // 1. DUAL 仮想テーブル
    let rows = conn.query("SELECT 100 + 200 FROM DUAL").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get_as::<i32>(0).unwrap(), 300);

    // 2. Oracle 特有のデータ型 (NUMBER, VARCHAR2, RAW, CLOB)
    conn.execute("CREATE TABLE ora_emp (id NUMBER, name VARCHAR2(100), note CLOB)").unwrap();
    conn.execute("INSERT INTO ora_emp VALUES (1, 'Oracle User', 'CLOB Data')").unwrap();
    let rows = conn.query("SELECT id, name FROM ora_emp WHERE id = 1").unwrap();
    assert_eq!(rows[0].get_as::<String>(1).unwrap(), "Oracle User");

    // 3. 空文字列 '' が NULL として評価されること (Oracle 固有仕様)
    let rows = conn.query("SELECT 1 FROM DUAL WHERE '' IS NULL").unwrap();
    assert_eq!(rows.len(), 1);

    let rows = conn.query("SELECT 1 FROM DUAL WHERE '' IS NOT NULL").unwrap();
    assert_eq!(rows.len(), 0);

    // NVL with empty string -> default value
    let rows = conn.query("SELECT NVL('', 'default_val') FROM DUAL").unwrap();
    assert_eq!(rows[0].get_as::<String>(0).unwrap(), "default_val");

    // 4. NVL, NVL2, DECODE
    let rows = conn.query("SELECT NVL(NULL, 'alt'), NVL('val', 'alt') FROM DUAL").unwrap();
    assert_eq!(rows[0].get_as::<String>(0).unwrap(), "alt");
    assert_eq!(rows[0].get_as::<String>(1).unwrap(), "val");

    let rows = conn.query("SELECT NVL2('not_null', 'is_val', 'is_null'), NVL2(NULL, 'is_val', 'is_null') FROM DUAL").unwrap();
    assert_eq!(rows[0].get_as::<String>(0).unwrap(), "is_val");
    assert_eq!(rows[0].get_as::<String>(1).unwrap(), "is_null");

    let rows = conn.query("SELECT DECODE(2, 1, 'one', 2, 'two', 'other') FROM DUAL").unwrap();
    assert_eq!(rows[0].get_as::<String>(0).unwrap(), "two");

    let rows = conn.query("SELECT DECODE(9, 1, 'one', 2, 'two', 'default_res') FROM DUAL").unwrap();
    assert_eq!(rows[0].get_as::<String>(0).unwrap(), "default_res");

    // DECODE matching NULL
    let rows = conn.query("SELECT DECODE(NULL, NULL, 'matched_null', 'not_matched') FROM DUAL").unwrap();
    assert_eq!(rows[0].get_as::<String>(0).unwrap(), "matched_null");

    // 5. SYSDATE, TO_CHAR, TO_DATE, TO_NUMBER
    let rows = conn.query("SELECT SYSDATE FROM DUAL").unwrap();
    assert_eq!(rows.len(), 1);

    let rows = conn.query("SELECT TO_CHAR(DATE '2026-09-26', 'YYYY/MM/DD') FROM DUAL").unwrap();
    assert_eq!(rows[0].get_as::<String>(0).unwrap(), "2026/09/26");

    let rows = conn.query("SELECT TO_DATE('2026-12-31', 'YYYY-MM-DD') FROM DUAL").unwrap();
    assert_eq!(rows.len(), 1);

    let rows = conn.query("SELECT TO_NUMBER('42.5') FROM DUAL").unwrap();
    assert_eq!(rows.len(), 1);
}

#[test]
fn test_mysql_compatibility_mode() {
    let conn = Connection::open_in_memory_with_mode(SqlDialectMode::MySql).unwrap();

    // 1. バッククォート識別子
    conn.execute("CREATE TABLE `my_table` (`col_id` INT, `user_name` VARCHAR(50), `created_at` DATETIME)").unwrap();
    conn.execute("INSERT INTO `my_table` VALUES (1, 'MySQLUser', '2026-09-26 12:00:00')").unwrap();

    let rows = conn.query("SELECT `user_name` FROM `my_table` WHERE `col_id` = 1").unwrap();
    assert_eq!(rows[0].get_as::<String>(0).unwrap(), "MySQLUser");

    // 2. IFNULL 関数
    let rows = conn.query("SELECT IFNULL(NULL, 'fallback'), IFNULL('hello', 'fallback')").unwrap();
    assert_eq!(rows[0].get_as::<String>(0).unwrap(), "fallback");
    assert_eq!(rows[0].get_as::<String>(1).unwrap(), "hello");

    // 3. IF(cond, then, else)
    let rows = conn.query("SELECT IF(1 > 0, 'yes', 'no'), IF(1 = 0, 'yes', 'no')").unwrap();
    assert_eq!(rows[0].get_as::<String>(0).unwrap(), "yes");
    assert_eq!(rows[0].get_as::<String>(1).unwrap(), "no");

    // 4. CURDATE, CURTIME, UNIX_TIMESTAMP, FROM_UNIXTIME
    let rows = conn.query("SELECT CURDATE(), CURTIME(), UNIX_TIMESTAMP()").unwrap();
    assert_eq!(rows.len(), 1);

    let rows = conn.query("SELECT FROM_UNIXTIME(1700000000)").unwrap();
    assert_eq!(rows.len(), 1);

    // 5. CONCAT_WS
    let rows = conn.query("SELECT CONCAT_WS('-', '2026', '09', '26')").unwrap();
    assert_eq!(rows[0].get_as::<String>(0).unwrap(), "2026-09-26");

    // 6. DATABASE(), SCHEMA(), VERSION()
    let rows = conn.query("SELECT DATABASE(), SCHEMA(), VERSION()").unwrap();
    assert_eq!(rows[0].get_as::<String>(0).unwrap(), "PUBLIC");
    assert_eq!(rows[0].get_as::<String>(1).unwrap(), "PUBLIC");
    assert!(rows[0].get_as::<String>(2).unwrap().contains("H2Database"));
}

#[test]
fn test_mssql_compatibility_mode() {
    let conn = Connection::open_in_memory_with_mode(SqlDialectMode::MsSqlServer).unwrap();

    // 1. 角括弧 `[identifier]` 構文
    conn.execute("CREATE TABLE [dbo_users] ([user_id] INT, [display_name] NVARCHAR(100), [salary] MONEY)").unwrap();
    conn.execute("INSERT INTO [dbo_users] VALUES (1, 'T-SQL User', 5000.50)").unwrap();

    let rows = conn.query("SELECT [display_name] FROM [dbo_users] WHERE [user_id] = 1").unwrap();
    assert_eq!(rows[0].get_as::<String>(0).unwrap(), "T-SQL User");

    // 2. 文字列連結 `+` 演算子 (SQL Server 特有)
    let rows = conn.query("SELECT 'Hello' + ' ' + 'World'").unwrap();
    assert_eq!(rows[0].get_as::<String>(0).unwrap(), "Hello World");

    let rows = conn.query("SELECT 'Prefix_' + 123").unwrap();
    assert_eq!(rows[0].get_as::<String>(0).unwrap(), "Prefix_123");

    // 3. ISNULL, GETDATE, LEN, CHARINDEX, NEWID, SQUARE
    let rows = conn.query("SELECT ISNULL(NULL, 'default_val')").unwrap();
    assert_eq!(rows[0].get_as::<String>(0).unwrap(), "default_val");

    let rows = conn.query("SELECT GETDATE()").unwrap();
    assert_eq!(rows.len(), 1);

    let rows = conn.query("SELECT LEN('RustDatabase')").unwrap();
    assert_eq!(rows[0].get_as::<i32>(0).unwrap(), 12);

    let rows = conn.query("SELECT CHARINDEX('Base', 'RustDataBase')").unwrap();
    assert_eq!(rows[0].get_as::<i32>(0).unwrap(), 9);

    let rows = conn.query("SELECT SQUARE(8)").unwrap();
    assert_eq!(rows[0].get_as::<i32>(0).unwrap(), 64);

    let rows = conn.query("SELECT NEWID()").unwrap();
    let uuid_str = rows[0].get_as::<String>(0).unwrap();
    assert_eq!(uuid_str.len(), 36);
}

#[test]
fn test_db2_compatibility_mode() {
    let conn = Connection::open_in_memory_with_mode(SqlDialectMode::Db2).unwrap();

    // 1. DUAL テーブルのサポート
    let rows = conn.query("SELECT 40 + 2 FROM DUAL").unwrap();
    assert_eq!(rows[0].get_as::<i32>(0).unwrap(), 42);

    // 2. ANSI 構文 & NVL / COALESCE
    let rows = conn.query("SELECT NVL(NULL, 999) FROM DUAL").unwrap();
    assert_eq!(rows[0].get_as::<i32>(0).unwrap(), 999);
}

#[test]
fn test_postgresql_compatibility_mode() {
    let conn = Connection::open_in_memory_with_mode(SqlDialectMode::PostgreSql).unwrap();

    // 1. :: キャスト構文
    let rows = conn.query("SELECT '123'::INTEGER, '45.67'::DOUBLE").unwrap();
    assert_eq!(rows[0].get_as::<i32>(0).unwrap(), 123);

    // 2. ILIKE 構文
    conn.execute("CREATE TABLE pg_items (name VARCHAR(50))").unwrap();
    conn.execute("INSERT INTO pg_items VALUES ('PostgresEngine')").unwrap();
    let rows = conn.query("SELECT name FROM pg_items WHERE name ILIKE '%gres%'").unwrap();
    assert_eq!(rows.len(), 1);
}
