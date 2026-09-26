use h2::{Connection, Value};
use tempfile::NamedTempFile;

/// 1. 任意本文の PL/pgSQL スカラー関数実行 (add_one)
#[test]
fn test_plpgsql_scalar_function_arbitrary_body() {
    let conn = Connection::open_in_memory().unwrap();

    let ddl = "
    CREATE FUNCTION add_one(x int) RETURNS int AS $$
    BEGIN
        RETURN x + 1;
    END;
    $$ LANGUAGE plpgsql;
    ";
    conn.execute(ddl).unwrap();

    // 単体呼出し
    let rows = conn.query("SELECT add_one(4)").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::BigInt(5));

    // 式内での合成呼出し
    let rows = conn.query("SELECT add_one(10) + add_one(20)").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::BigInt(32)); // 11 + 21
}

/// 2. 未定義手続き呼出しのエラー（P0 課題: 偽の成功の撤廃検証）
#[test]
fn test_plpgsql_undefined_routine_fails() {
    let conn = Connection::open_in_memory().unwrap();

    // 未知の手続き CALL はエラーになること
    let res = conn.execute("CALL does_not_exist()");
    assert!(res.is_err(), "CALL does_not_exist() must return an error");

    // 未知の関数呼出しもエラーになること
    let res = conn.query("SELECT does_not_exist(1)");
    assert!(res.is_err(), "SELECT does_not_exist() must return an error");
}

/// 3. DROP FUNCTION / DROP PROCEDURE の厳格な検査（P0 課題解消）
#[test]
fn test_plpgsql_drop_routine() {
    let conn = Connection::open_in_memory().unwrap();

    let ddl = "
    CREATE PROCEDURE dummy_proc() LANGUAGE plpgsql AS $$
    BEGIN
        NULL;
    END;
    $$;
    ";
    conn.execute(ddl).unwrap();

    // 存在時は CALL 成功
    conn.execute("CALL dummy_proc()").unwrap();

    // 削除実行
    conn.execute("DROP PROCEDURE dummy_proc()").unwrap();

    // 削除後は CALL がエラーになること
    let call_res = conn.execute("CALL dummy_proc()");
    assert!(call_res.is_err(), "CALL dropped procedure must return error");

    // 再度の DROP は存在しないためエラーになること
    let drop_res = conn.execute("DROP PROCEDURE dummy_proc()");
    assert!(drop_res.is_err(), "DROP non-existent procedure must return error");

    // IF EXISTS を指定すれば成功すること
    conn.execute("DROP PROCEDURE IF EXISTS dummy_proc()").unwrap();
}

/// 4. 制御フロー構文 (IF / WHILE / FOR / LOOP / EXIT / CONTINUE)
#[test]
fn test_plpgsql_control_flow() {
    let conn = Connection::open_in_memory().unwrap();

    // IF / ELSIF / ELSE
    let ddl_if = "
    CREATE FUNCTION sign_of(n int) RETURNS int AS $$
    BEGIN
        IF n > 0 THEN
            RETURN 1;
        ELSIF n < 0 THEN
            RETURN -1;
        ELSE
            RETURN 0;
        END IF;
    END;
    $$ LANGUAGE plpgsql;
    ";
    conn.execute(ddl_if).unwrap();

    assert_eq!(conn.query("SELECT sign_of(10)").unwrap()[0].values[0], Value::BigInt(1));
    assert_eq!(conn.query("SELECT sign_of(-42)").unwrap()[0].values[0], Value::BigInt(-1));
    assert_eq!(conn.query("SELECT sign_of(0)").unwrap()[0].values[0], Value::BigInt(0));

    // WHILE ループ
    let ddl_while = "
    CREATE FUNCTION sum_while(max_n int) RETURNS int AS $$
    DECLARE
        total int := 0;
        i int := 1;
    BEGIN
        WHILE i <= max_n LOOP
            total := total + i;
            i := i + 1;
        END LOOP;
        RETURN total;
    END;
    $$ LANGUAGE plpgsql;
    ";
    conn.execute(ddl_while).unwrap();
    assert_eq!(conn.query("SELECT sum_while(10)").unwrap()[0].values[0], Value::BigInt(55));

    // FOR IN 範囲ループ
    let ddl_for = "
    CREATE FUNCTION sum_for(start_n int, end_n int) RETURNS int AS $$
    DECLARE
        total int := 0;
    BEGIN
        FOR i IN start_n..end_n LOOP
            total := total + i;
        END LOOP;
        RETURN total;
    END;
    $$ LANGUAGE plpgsql;
    ";
    conn.execute(ddl_for).unwrap();
    assert_eq!(conn.query("SELECT sum_for(1, 5)").unwrap()[0].values[0], Value::BigInt(15)); // 1+2+3+4+5

    // LOOP + EXIT WHEN
    let ddl_loop = "
    CREATE FUNCTION loop_exit(limit_val int) RETURNS int AS $$
    DECLARE
        count int := 0;
    BEGIN
        LOOP
            count := count + 1;
            EXIT WHEN count >= limit_val;
        END LOOP;
        RETURN count;
    END;
    $$ LANGUAGE plpgsql;
    ";
    conn.execute(ddl_loop).unwrap();
    assert_eq!(conn.query("SELECT loop_exit(7)").unwrap()[0].values[0], Value::BigInt(7));
}

/// 5. SELECT ... INTO と DML (INSERT, UPDATE) と変数置換
#[test]
fn test_plpgsql_select_into_and_dml() {
    let conn = Connection::open_in_memory().unwrap();

    conn.execute("CREATE TABLE accounts (id INT PRIMARY KEY, balance DECIMAL(10,2))").unwrap();
    conn.execute("INSERT INTO accounts VALUES (1, 1000.00), (2, 500.00)").unwrap();

    let ddl_transfer = "
    CREATE PROCEDURE transfer_funds(acc_id INT, amount DECIMAL(10,2)) LANGUAGE plpgsql AS $$
    DECLARE
        current_bal DECIMAL(10,2);
    BEGIN
        SELECT balance INTO current_bal FROM accounts WHERE id = acc_id;
        current_bal := current_bal + amount;
        UPDATE accounts SET balance = current_bal WHERE id = acc_id;
    END;
    $$;
    ";
    conn.execute(ddl_transfer).unwrap();

    // 送金実行
    conn.execute("CALL transfer_funds(1, 250.00)").unwrap();

    let rows = conn.query("SELECT balance FROM accounts WHERE id = 1").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::Decimal(rust_decimal::Decimal::new(125000, 2)));
}

/// 6. RAISE EXCEPTION と トランザクションロールバック
#[test]
fn test_plpgsql_raise_exception_and_rollback() {
    let conn = Connection::open_in_memory().unwrap();

    conn.execute("CREATE TABLE items (id INT PRIMARY KEY, name VARCHAR)").unwrap();

    let ddl_fail = "
    CREATE PROCEDURE fail_after_insert(item_id INT) LANGUAGE plpgsql AS $$
    BEGIN
        INSERT INTO items VALUES (item_id, 'Temporary');
        RAISE EXCEPTION 'Simulated abort';
    END;
    $$;
    ";
    conn.execute(ddl_fail).unwrap();

    // トランザクション内で実行
    conn.execute("BEGIN").unwrap();
    conn.execute("INSERT INTO items VALUES (1, 'Initial')").unwrap();

    let res = conn.execute("CALL fail_after_insert(2)");
    assert!(res.is_err(), "Procedure with RAISE EXCEPTION must error");

    conn.execute("ROLLBACK").unwrap();

    // ロールバックされたのでテーブルは空
    let rows = conn.query("SELECT count(*) FROM items").unwrap();
    assert_eq!(rows[0].values[0], Value::BigInt(0));
}

/// 7. DO 匿名コードブロック
#[test]
fn test_plpgsql_anonymous_do_block() {
    let conn = Connection::open_in_memory().unwrap();

    conn.execute("CREATE TABLE messages (msg VARCHAR)").unwrap();

    let do_sql = "
    DO $$
    DECLARE
        greeting VARCHAR := 'Hello from anonymous PL/pgSQL block!';
    BEGIN
        INSERT INTO messages VALUES (greeting);
    END;
    $$;
    ";
    conn.execute(do_sql).unwrap();

    let rows = conn.query("SELECT msg FROM messages").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::String("Hello from anonymous PL/pgSQL block!".to_string()));
}

/// 8. pg_proc カタログ照会（P1 課題解消: 実際の登録状態に基づくカタログ情報）
#[test]
fn test_plpgsql_pg_proc_catalog_reflection() {
    let conn = Connection::open_in_memory().unwrap();

    // ユーザー定義関数を作成
    let ddl = "
    CREATE FUNCTION my_custom_calc(val int) RETURNS int AS $$
    BEGIN
        RETURN val * 2;
    END;
    $$ LANGUAGE plpgsql;
    ";
    conn.execute(ddl).unwrap();

    // pg_proc からの検索（WHERE proname = 'my_custom_calc'）
    let rows = conn.query("SELECT proname, prokind, prorettype, pronargs FROM pg_proc WHERE proname = 'my_custom_calc'").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].values[0], Value::String("my_custom_calc".to_string()));
    assert_eq!(rows[0].values[1], Value::String("f".to_string()));
    assert_eq!(rows[0].values[2], Value::String("integer".to_string()));
    assert_eq!(rows[0].values[3], Value::Integer(1));

    // プロシージャも作成
    conn.execute("CREATE PROCEDURE my_custom_proc() LANGUAGE plpgsql AS $$ BEGIN NULL; END $$;").unwrap();

    // pg_proc 集計クエリ（prokind = 'p'）
    let proc_rows = conn.query("SELECT p.prokind, count(*) AS cnt FROM pg_proc p WHERE p.prokind = 'p' GROUP BY p.prokind").unwrap();
    assert_eq!(proc_rows.len(), 1);
    assert_eq!(proc_rows[0].values[0], Value::String("p".to_string()));
    assert_eq!(proc_rows[0].values[1], Value::BigInt(1)); // my_custom_proc
}

/// 9. HammerDB ワークロードが暫定シミュレータなしで真に機能することの検証
#[test]
fn test_hammerdb_provisional_simulator_obsolete_and_functional() {
    let conn = Connection::open_in_memory().unwrap();

    // 1. DDL: DBMS_RANDOM 関数の定義（ALIAS、random()、trunc() を含む任意本文）
    let ddl_func = "
    CREATE OR REPLACE FUNCTION DBMS_RANDOM (INTEGER, INTEGER) RETURNS INTEGER AS $$
    DECLARE
        start_int ALIAS FOR $1;
        end_int ALIAS FOR $2;
    BEGIN
        RETURN trunc(random() * (end_int - start_int + 1) + start_int);
    END;
    $$ LANGUAGE 'plpgsql' STRICT;
    ";
    conn.execute(ddl_func).unwrap();

    // 2. DDL: NEWORD プロシージャの定義（IN / INOUT パラメータ付き）
    let ddl_proc = "
    CREATE OR REPLACE PROCEDURE NEWORD (
        no_w_id         IN INTEGER,
        no_max_w_id     IN INTEGER,
        no_d_id         IN INTEGER,
        no_c_id         IN INTEGER,
        no_o_ol_cnt     IN INTEGER,
        no_c_discount   INOUT NUMERIC,
        no_c_last       INOUT VARCHAR,
        no_c_credit     INOUT VARCHAR,
        no_d_tax        INOUT NUMERIC,
        no_w_tax        INOUT NUMERIC,
        no_d_next_o_id  INOUT INTEGER,
        tstamp          IN TIMESTAMP
    ) AS $$
    BEGIN
        no_c_discount := 0.0825;
        no_c_last := 'BAR';
        no_d_next_o_id := 3001;
    END;
    $$ LANGUAGE 'plpgsql';
    ";
    conn.execute(ddl_proc).unwrap();

    // 3. pg_proc カタログ照会（HammerDB がスキーマ検査で投げるクエリ）
    let proc_rows = conn.query("SELECT p.prokind, count(*) AS cnt FROM pg_proc p WHERE p.prokind = 'p' GROUP BY p.prokind").unwrap();
    assert_eq!(proc_rows.len(), 1);
    assert_eq!(proc_rows[0].values[0], Value::String("p".to_string()));
    assert_eq!(proc_rows[0].values[1], Value::BigInt(1)); // neword

    let func_rows = conn.query("SELECT p.prokind, count(*) AS cnt FROM pg_proc p WHERE p.prokind = 'f' GROUP BY p.prokind").unwrap();
    assert_eq!(func_rows.len(), 1);
    assert_eq!(func_rows[0].values[0], Value::String("f".to_string()));
    assert_eq!(func_rows[0].values[1], Value::BigInt(1)); // dbms_random

    // 4. SELECT DBMS_RANDOM(10, 20) を SQL 式評価・手続き型エンジンで実実行
    for _ in 0..10 {
        let rows = conn.query("SELECT DBMS_RANDOM(10, 20)").unwrap();
        assert_eq!(rows.len(), 1);
        if let Value::BigInt(val) = rows[0].values[0] {
            assert!((10..=20).contains(&val), "Generated random {} not in 10..=20", val);
        } else if let Value::Integer(val) = rows[0].values[0] {
            assert!((10..=20).contains(&(val as i64)), "Generated random {} not in 10..=20", val);
        } else {
            panic!("Expected integer or bigint result from DBMS_RANDOM, got {:?}", rows[0].values[0]);
        }
    }

    // 5. CALL neword(...) を実実行（INOUT 引数が更新されて結果セットとして返却されること）
    let call_rows = conn.query("CALL neword(1, 1, 1, 100, 10, 0.0, '', '', 0.0, 0.0, 0, CURRENT_TIMESTAMP)").unwrap();
    assert_eq!(call_rows.len(), 1);
    // INOUT 引数が 6 つ返却される
    assert_eq!(call_rows[0].values.len(), 6);
    // no_c_last = 'BAR'
    assert_eq!(call_rows[0].values[1], Value::String("BAR".to_string()));
    // no_d_next_o_id = 3001
    assert_eq!(call_rows[0].values[5], Value::BigInt(3001));
}

/// 9. データベース再起動後の永続性検証
#[test]
fn test_plpgsql_persistence_across_restart() {
    let tmp_file = NamedTempFile::new().unwrap();
    let db_path = tmp_file.path().to_str().unwrap().to_string();

    {
        let conn = Connection::open(&db_path).unwrap();
        let ddl = "
        CREATE FUNCTION square(x int) RETURNS int AS $$
        BEGIN
            RETURN x * x;
        END;
        $$ LANGUAGE plpgsql;
        ";
        conn.execute(ddl).unwrap();

        let rows = conn.query("SELECT square(5)").unwrap();
        assert_eq!(rows[0].values[0], Value::BigInt(25));
    }

    // コネクションを閉じ、再度開いて永続化されたルーチンを呼び出す
    {
        let conn = Connection::open(&db_path).unwrap();
        let rows = conn.query("SELECT square(6)").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].values[0], Value::BigInt(36));
    }
}
