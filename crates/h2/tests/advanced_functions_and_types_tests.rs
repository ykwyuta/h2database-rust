use h2::Connection;

#[test]
fn test_advanced_math_functions() {
    let conn = Connection::open_in_memory().unwrap();

    let rows = conn.query("SELECT ABS(-15), SIGN(-20), SIGN(0), SIGN(42), PI()").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get_as::<i32>(0).unwrap(), 15);
    assert_eq!(rows[0].get_as::<i32>(1).unwrap(), -1);
    assert_eq!(rows[0].get_as::<i32>(2).unwrap(), 0);
    assert_eq!(rows[0].get_as::<i32>(3).unwrap(), 1);
    let pi_val = rows[0].get_as::<f64>(4).unwrap();
    assert!((pi_val - std::f64::consts::PI).abs() < 1e-9);

    // 三角関数
    let rows = conn.query("SELECT SIN(0), COS(0), TAN(0), ROUND(DEGREES(PI()), 0), RADIANS(180)").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get_as::<f64>(0).unwrap(), 0.0);
    assert_eq!(rows[0].get_as::<f64>(1).unwrap(), 1.0);
    assert_eq!(rows[0].get_as::<f64>(2).unwrap(), 0.0);
    assert_eq!(rows[0].get_as::<i32>(3).unwrap(), 180);

    // 指数・対数・平方根・累乗
    let rows = conn.query("SELECT SQRT(16), POWER(2, 8), POW(3, 3), ROUND(EXP(1), 4), ROUND(LN(EXP(2)), 4), LOG10(1000)").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get_as::<f64>(0).unwrap(), 4.0);
    assert_eq!(rows[0].get_as::<f64>(1).unwrap(), 256.0);
    assert_eq!(rows[0].get_as::<f64>(2).unwrap(), 27.0);
    assert_eq!(rows[0].get_as::<f64>(5).unwrap(), 3.0);

    // 丸め・切り捨て・剰余
    let rows = conn.query("SELECT CEIL(4.2), FLOOR(4.8), ROUND(123.456, 2), TRUNC(123.456, 2), MOD(17, 5), 17 % 5").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get_as::<f64>(0).unwrap(), 5.0);
    assert_eq!(rows[0].get_as::<f64>(1).unwrap(), 4.0);
    assert_eq!(rows[0].get_as::<f64>(2).unwrap(), 123.46);
    assert_eq!(rows[0].get_as::<f64>(3).unwrap(), 123.45);
    assert_eq!(rows[0].get_as::<i32>(4).unwrap(), 2);
    assert_eq!(rows[0].get_as::<i32>(5).unwrap(), 2);
}

#[test]
fn test_advanced_string_functions() {
    let conn = Connection::open_in_memory().unwrap();

    let rows = conn.query("SELECT SUBSTR('Hello World', 7, 5), SUBSTRING('Database' FROM 1 FOR 4)").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get_as::<String>(0).unwrap(), "World");
    assert_eq!(rows[0].get_as::<String>(1).unwrap(), "Data");

    let rows = conn.query("SELECT LTRIM('   abc'), RTRIM('abc   '), BTRIM('  abc  '), TRIM('  abc  ')").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get_as::<String>(0).unwrap(), "abc");
    assert_eq!(rows[0].get_as::<String>(1).unwrap(), "abc");
    assert_eq!(rows[0].get_as::<String>(2).unwrap(), "abc");
    assert_eq!(rows[0].get_as::<String>(3).unwrap(), "abc");

    let rows = conn.query("SELECT LPAD('123', 6, '0'), RPAD('abc', 6, '-'), INITCAP('hello world test'), REVERSE('Rust'), REPEAT('Ab', 3)").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get_as::<String>(0).unwrap(), "000123");
    assert_eq!(rows[0].get_as::<String>(1).unwrap(), "abc---");
    assert_eq!(rows[0].get_as::<String>(2).unwrap(), "Hello World Test");
    assert_eq!(rows[0].get_as::<String>(3).unwrap(), "tsuR");
    assert_eq!(rows[0].get_as::<String>(4).unwrap(), "AbAbAb");

    let rows = conn.query("SELECT REPLACE('foo bar foo', 'foo', 'baz'), TRANSLATE('12345', '14', 'ax'), SPLIT_PART('a/b/c/d', '/', 3), LEFT('Database', 4), RIGHT('Database', 4), 'foo' || 'bar'").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get_as::<String>(0).unwrap(), "baz bar baz");
    assert_eq!(rows[0].get_as::<String>(1).unwrap(), "a23x5");
    assert_eq!(rows[0].get_as::<String>(2).unwrap(), "c");
    assert_eq!(rows[0].get_as::<String>(3).unwrap(), "Data");
    assert_eq!(rows[0].get_as::<String>(4).unwrap(), "base");
    assert_eq!(rows[0].get_as::<String>(5).unwrap(), "foobar");

    let rows = conn.query("SELECT CHR(65), ASCII('A')").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get_as::<String>(0).unwrap(), "A");
    assert_eq!(rows[0].get_as::<i32>(1).unwrap(), 65);
}

#[test]
fn test_regex_operators_and_functions() {
    let conn = Connection::open_in_memory().unwrap();

    let rows = conn.query("SELECT 'hello world' ~ '^hello', 'hello world' ~ '^world', 'HELLO' ~* '^hello', 'hello' !~ '^[0-9]+'").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get_as::<bool>(0).unwrap(), true);
    assert_eq!(rows[0].get_as::<bool>(1).unwrap(), false);
    assert_eq!(rows[0].get_as::<bool>(2).unwrap(), true);
    assert_eq!(rows[0].get_as::<bool>(3).unwrap(), true);

    let rows = conn.query("SELECT REGEXP_LIKE('user123', '^[a-z]+[0-9]+$'), REGEXP_REPLACE('2025-09-25', '-', '/', 'g'), REGEXP_SUBSTR('order-4567-item', '[0-9]+')").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get_as::<bool>(0).unwrap(), true);
    assert_eq!(rows[0].get_as::<String>(1).unwrap(), "2025/09/25");
    assert_eq!(rows[0].get_as::<String>(2).unwrap(), "4567");
}

#[test]
fn test_date_functions_and_interval() {
    let conn = Connection::open_in_memory().unwrap();

    // EXTRACT
    let rows = conn.query("SELECT EXTRACT(YEAR FROM DATE '2025-09-25'), EXTRACT(MONTH FROM DATE '2025-09-25'), EXTRACT(DAY FROM DATE '2025-09-25')").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get_as::<i32>(0).unwrap(), 2025);
    assert_eq!(rows[0].get_as::<i32>(1).unwrap(), 9);
    assert_eq!(rows[0].get_as::<i32>(2).unwrap(), 25);

    // DATE_PART, DATE_TRUNC
    let rows = conn.query("SELECT DATE_PART('year', DATE '2025-09-25'), DATE_TRUNC('month', DATE '2025-09-25')").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get_as::<i32>(0).unwrap(), 2025);
    assert_eq!(rows[0].get_as::<chrono::NaiveDate>(1).unwrap(), chrono::NaiveDate::from_ymd_opt(2025, 9, 1).unwrap());

    // INTERVAL 演算
    let rows = conn.query("SELECT DATE '2025-01-01' + INTERVAL '10 days', DATE '2025-01-10' - INTERVAL '5 days', DATE '2025-01-10' - DATE '2025-01-01'").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get_as::<chrono::NaiveDate>(0).unwrap(), chrono::NaiveDate::from_ymd_opt(2025, 1, 11).unwrap());
    assert_eq!(rows[0].get_as::<chrono::NaiveDate>(1).unwrap(), chrono::NaiveDate::from_ymd_opt(2025, 1, 5).unwrap());

    // DATE_ADD, DATE_SUB, DATEDIFF
    let rows = conn.query("SELECT DATE_ADD(DATE '2025-01-01', INTERVAL '1 month'), DATE_SUB(DATE '2025-02-01', INTERVAL '5 days'), DATEDIFF('day', DATE '2025-01-10', DATE '2025-01-01')").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get_as::<chrono::NaiveDate>(0).unwrap(), chrono::NaiveDate::from_ymd_opt(2025, 2, 1).unwrap());
    assert_eq!(rows[0].get_as::<chrono::NaiveDate>(1).unwrap(), chrono::NaiveDate::from_ymd_opt(2025, 1, 27).unwrap());
    assert_eq!(rows[0].get_as::<i32>(2).unwrap(), 9);
}
