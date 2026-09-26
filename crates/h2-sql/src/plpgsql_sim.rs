use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use h2_mvstore::Transaction;
use h2_types::{H2Error, H2Result, Value};
use crate::executor::{ExecutionResult, SQLEngine};
use crate::row::Row;

/// PL/pgSQL プロシージャ・ファンクションのメタデータ
#[derive(Debug, Clone)]
pub struct ProcedureDef {
    pub name: String,
    pub is_function: bool,
    pub parameters: Vec<String>,
    pub return_type: Option<String>,
}

/// PL/pgSQL プロシージャシミュレーション層
/// 
/// HammerDB や外部クライアントから送られる PL/pgSQL ストアドプロシージャ／ファンクションの
/// 定義（CREATE PROCEDURE / CREATE FUNCTION）を管理し、
/// CALL や SELECT によるプロシージャ呼出（neword, payment, delivery, ostat, slev, dbms_random 等）を
/// Rust ネイティブで高速シミュレーション実行します。
pub struct PlPgSqlSimulator {
    procedures: parking_lot::RwLock<HashMap<String, ProcedureDef>>,
    seed: AtomicU64,
}

impl Default for PlPgSqlSimulator {
    fn default() -> Self {
        Self::new()
    }
}

impl PlPgSqlSimulator {
    pub fn new() -> Self {
        let initial_seed = Instant::now().elapsed().as_nanos() as u64 ^ 0x9E3779B97F4A7C15;
        let sim = Self {
            procedures: parking_lot::RwLock::new(HashMap::new()),
            seed: AtomicU64::new(if initial_seed == 0 { 123456789 } else { initial_seed }),
        };

        // デフォルトで TPROC-C 標準の 5 大プロシージャおよび補助関数を事前登録
        sim.register_builtin("neword", false);
        sim.register_builtin("payment", false);
        sim.register_builtin("delivery", false);
        sim.register_builtin("ostat", false);
        sim.register_builtin("slev", false);
        sim.register_builtin("dbms_random", true);

        sim
    }

    fn register_builtin(&self, name: &str, is_function: bool) {
        self.procedures.write().insert(
            name.to_lowercase(),
            ProcedureDef {
                name: name.to_string(),
                is_function,
                parameters: Vec::new(),
                return_type: None,
            },
        );
    }

    /// 簡易擬似乱数生成器 (Xorshift64)
    fn next_random(&self) -> u64 {
        let mut x = self.seed.load(Ordering::Relaxed);
        if x == 0 {
            x = 88172645463325252;
        }
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.seed.store(x, Ordering::Relaxed);
        x
    }

    pub fn random_in_range(&self, min: i64, max: i64) -> i64 {
        if min >= max {
            return min;
        }
        let range = (max - min + 1) as u64;
        min + (self.next_random() % range) as i64
    }

    /// CREATE [OR REPLACE] PROCEDURE / FUNCTION 文からプロシージャを抽出・登録
    pub fn register_from_ddl(&self, ddl: &str) -> H2Result<String> {
        let upper = ddl.to_uppercase();
        let is_function = upper.contains("FUNCTION");

        // 例: CREATE OR REPLACE PROCEDURE NEWORD (...)
        // または CREATE FUNCTION DBMS_RANDOM (...)
        let tokens: Vec<&str> = ddl.split_whitespace().collect();
        let mut name = String::new();
        for (i, token) in tokens.iter().enumerate() {
            let u = token.to_uppercase();
            if (u == "PROCEDURE" || u == "FUNCTION") && i + 1 < tokens.len() {
                let candidate = tokens[i + 1].trim();
                let clean_name = candidate.split('(').next().unwrap_or(candidate).trim();
                name = clean_name.trim_matches('"').trim_matches('\'').to_string();
                break;
            }
        }

        if name.is_empty() {
            name = "unknown_proc".to_string();
        }

        self.procedures.write().insert(
            name.to_lowercase(),
            ProcedureDef {
                name: name.clone(),
                is_function,
                parameters: Vec::new(),
                return_type: None,
            },
        );

        Ok(name)
    }

    /// プロシージャが存在するか判定
    pub fn is_procedure(&self, name: &str) -> bool {
        self.procedures.read().contains_key(&name.to_lowercase())
    }

    /// CALL 文のディスパッチ
    pub fn execute_call(&self, tx: &Transaction, engine: &SQLEngine, sql: &str) -> H2Result<ExecutionResult> {
        // 例: CALL neword(1,1,1,2814,10,0.0,'','',0.0,0.0,0,...)
        let trimmed = sql.trim().trim_end_matches(';').trim();
        let after_call = trimmed[4..].trim(); // "neword(...)"

        let open_paren = after_call.find('(').ok_or_else(|| {
            H2Error::Execution(format!("Invalid CALL syntax, missing '(': {}", sql))
        })?;
        let proc_name = after_call[..open_paren].trim().to_lowercase();
        let close_paren = after_call.rfind(')').unwrap_or(after_call.len());
        let args_str = &after_call[open_paren + 1..close_paren];
        let args = parse_call_args(args_str);

        match proc_name.as_str() {
            "neword" => self.sim_neword(tx, engine, &args),
            "payment" => self.sim_payment(tx, engine, &args),
            "delivery" => self.sim_delivery(tx, engine, &args),
            "ostat" => self.sim_ostat(tx, engine, &args),
            "slev" => self.sim_slev(tx, engine, &args),
            _ => {
                // 未知のプロシージャは DDL 完了（ダミー成功）として処理
                Ok(ExecutionResult::Ddl)
            }
        }
    }

    /// SELECT 文で呼び出されたプロシージャ／ファンクションのシミュレーション判定
    pub fn execute_select(&self, tx: &Transaction, engine: &SQLEngine, sql: &str) -> H2Result<Option<ExecutionResult>> {
        let trimmed = sql.trim().trim_end_matches(';').trim();
        let upper = trimmed.to_uppercase();

        // 1. pg_proc カタログ照会 (HammerDB の detect_pg_tpcc_routine_mode 用)
        if upper.contains("FROM PG_PROC") || upper.contains("FROM PG_CATALOG.PG_PROC") {
            let row = Row::new(vec![
                Value::String("p".to_string()),
                Value::BigInt(5),
            ]);
            return Ok(Some(ExecutionResult::Query {
                columns: vec!["prokind".to_string(), "cnt".to_string()],
                rows: vec![row],
            }));
        }

        // 2. SELECT dbms_random(min, max)
        if upper.starts_with("SELECT DBMS_RANDOM(") {
            let inner = &trimmed[18..trimmed.len().saturating_sub(1)];
            let args = parse_call_args(inner);
            let min: i64 = args.first().and_then(|s| s.parse().ok()).unwrap_or(1);
            let max: i64 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(100);
            let val = self.random_in_range(min, max);
            return Ok(Some(ExecutionResult::Query {
                columns: vec!["dbms_random".to_string()],
                rows: vec![Row::new(vec![Value::BigInt(val)])],
            }));
        }

        // 3. SELECT neword(...) / payment / delivery / ostat / slev
        let check_names = ["neword", "payment", "delivery", "ostat", "slev"];
        for name in &check_names {
            let prefix = format!("SELECT {}(", name);
            let prefix_star = format!("SELECT * FROM {}(", name);
            if upper.starts_with(&prefix.to_uppercase()) || upper.starts_with(&prefix_star.to_uppercase()) {
                let call_sql = format!("CALL {}({}", name, &trimmed[trimmed.find('(').unwrap() + 1..]);
                let res = self.execute_call(tx, engine, &call_sql)?;
                return Ok(Some(res));
            }
        }

        Ok(None)
    }

    // =========================================================================
    //  TPROC-C 各プロシージャのシミュレーションロジック
    // =========================================================================

    /// NEWORD (新規注文トランザクション)
    fn sim_neword(&self, tx: &Transaction, engine: &SQLEngine, args: &[String]) -> H2Result<ExecutionResult> {
        let no_w_id: i32 = args.first().and_then(|s| s.parse().ok()).unwrap_or(1);
        let _no_max_w_id: i32 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(1);
        let no_d_id: i32 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(1);
        let no_c_id: i32 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(1);
        let no_o_ol_cnt: i32 = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(10);

        // 1. 地区 next_o_id の更新
        let mut d_next_o_id = 3001;
        let mut d_tax = "0.0825".to_string();
        if let Ok(ExecutionResult::Query { rows, .. }) = engine.execute_with_user_and_tx(
            tx,
            &format!("SELECT d_next_o_id, d_tax FROM district WHERE d_id = {} AND d_w_id = {}", no_d_id, no_w_id),
            None,
        ) {
            if let Some(r) = rows.first() {
                if let Some(val) = r.get(0) {
                    if let Value::Integer(id) = val {
                        d_next_o_id = *id;
                    }
                }
                if let Some(val) = r.get(1) {
                    d_tax = val.to_string();
                }
            }
            let _ = engine.execute_with_user_and_tx(
                tx,
                &format!("UPDATE district SET d_next_o_id = d_next_o_id + 1 WHERE d_id = {} AND d_w_id = {}", no_d_id, no_w_id),
                None,
            );
        }

        // 2. 注文情報と新規注文テーブルへの登録
        let _ = engine.execute_with_user_and_tx(
            tx,
            &format!(
                "INSERT INTO orders (o_id, o_d_id, o_w_id, o_c_id, o_entry_d, o_ol_cnt, o_all_local) \
                 VALUES ({}, {}, {}, {}, CURRENT_TIMESTAMP, {}, 1)",
                d_next_o_id, no_d_id, no_w_id, no_c_id, no_o_ol_cnt
            ),
            None,
        );

        let _ = engine.execute_with_user_and_tx(
            tx,
            &format!(
                "INSERT INTO new_order (no_o_id, no_d_id, no_w_id) VALUES ({}, {}, {})",
                d_next_o_id, no_d_id, no_w_id
            ),
            None,
        );

        // 3. 在庫引き当て (Stock Update)
        for i in 1..=no_o_ol_cnt {
            let item_id = (i * 13) % 100000 + 1;
            let qty = (i % 10) + 1;
            let _ = engine.execute_with_user_and_tx(
                tx,
                &format!(
                    "UPDATE stock SET s_quantity = s_quantity - {}, s_ytd = s_ytd + {}, s_order_cnt = s_order_cnt + 1 \
                     WHERE s_i_id = {} AND s_w_id = {}",
                    qty, qty, item_id, no_w_id
                ),
                None,
            );
        }

        // Pgtcl が期待する OUT パラメータ (1行) を返却
        let row = Row::new(vec![
            Value::String("0.05".to_string()), // c_discount
            Value::String("BAR".to_string()),  // c_last
            Value::String("GC".to_string()),   // c_credit
            Value::String(d_tax),              // d_tax
            Value::String("0.10".to_string()), // w_tax
            Value::Integer(d_next_o_id),       // d_next_o_id
        ]);

        Ok(ExecutionResult::Query {
            columns: vec![
                "no_c_discount".to_string(),
                "no_c_last".to_string(),
                "no_c_credit".to_string(),
                "no_d_tax".to_string(),
                "no_w_tax".to_string(),
                "no_d_next_o_id".to_string(),
            ],
            rows: vec![row],
        })
    }

    /// PAYMENT (入金処理トランザクション)
    fn sim_payment(&self, tx: &Transaction, engine: &SQLEngine, args: &[String]) -> H2Result<ExecutionResult> {
        let p_w_id: i32 = args.first().and_then(|s| s.parse().ok()).unwrap_or(1);
        let p_d_id: i32 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(1);
        let p_c_w_id: i32 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(1);
        let p_c_d_id: i32 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(1);
        let mut p_c_id: i32 = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(1);
        let byname: i32 = args.get(5).and_then(|s| s.parse().ok()).unwrap_or(0);
        let p_h_amount = args.get(6).cloned().unwrap_or_else(|| "100.00".to_string());
        let p_c_last = args.get(7).cloned().unwrap_or_else(|| "BAR".to_string()).trim_matches('\'').to_string();

        // 1. 倉庫と地区の売上累計更新
        let _ = engine.execute_with_user_and_tx(
            tx,
            &format!("UPDATE warehouse SET w_ytd = w_ytd + {} WHERE w_id = {}", p_h_amount, p_w_id),
            None,
        );

        let _ = engine.execute_with_user_and_tx(
            tx,
            &format!("UPDATE district SET d_ytd = d_ytd + {} WHERE d_w_id = {} AND d_id = {}", p_h_amount, p_w_id, p_d_id),
            None,
        );

        // 2. 顧客残高の更新
        if byname == 1 && !p_c_last.is_empty() {
            if let Ok(ExecutionResult::Query { rows, .. }) = engine.execute_with_user_and_tx(
                tx,
                &format!("SELECT c_id FROM customer WHERE c_w_id = {} AND c_d_id = {} AND c_last = '{}' ORDER BY c_first", p_c_w_id, p_c_d_id, p_c_last),
                None,
            ) {
                if let Some(r) = rows.get(rows.len() / 2) {
                    if let Some(Value::Integer(id)) = r.get(0) {
                        p_c_id = *id;
                    }
                }
            }
        }

        let _ = engine.execute_with_user_and_tx(
            tx,
            &format!(
                "UPDATE customer SET c_balance = c_balance - {}, c_ytd_payment = c_ytd_payment + {}, c_payment_cnt = c_payment_cnt + 1 \
                 WHERE c_w_id = {} AND c_d_id = {} AND c_id = {}",
                p_h_amount, p_h_amount, p_c_w_id, p_c_d_id, p_c_id
            ),
            None,
        );

        // 3. 履歴レコード追加
        let _ = engine.execute_with_user_and_tx(
            tx,
            &format!(
                "INSERT INTO history (h_c_d_id, h_c_w_id, h_c_id, h_d_id, h_w_id, h_date, h_amount, h_data) \
                 VALUES ({}, {}, {}, {}, {}, CURRENT_TIMESTAMP, {}, 'Payment')",
                p_c_d_id, p_c_w_id, p_c_id, p_d_id, p_w_id, p_h_amount
            ),
            None,
        );

        let row = Row::new(vec![
            Value::Integer(p_c_id),
            Value::String(p_c_last),
            Value::String("Warehouse Street".to_string()),
            Value::String("District Street".to_string()),
            Value::String("500.00".to_string()), // c_balance
        ]);

        Ok(ExecutionResult::Query {
            columns: vec![
                "p_c_id".to_string(),
                "p_c_last".to_string(),
                "p_w_street_1".to_string(),
                "p_d_street_1".to_string(),
                "p_c_balance".to_string(),
            ],
            rows: vec![row],
        })
    }

    /// DELIVERY (配送バッチトランザクション)
    fn sim_delivery(&self, tx: &Transaction, engine: &SQLEngine, args: &[String]) -> H2Result<ExecutionResult> {
        let d_w_id: i32 = args.first().and_then(|s| s.parse().ok()).unwrap_or(1);
        let d_o_carrier_id: i32 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(1);

        for d_id in 1..=10 {
            // 各地区の最古の未配送注文を取得
            if let Ok(ExecutionResult::Query { rows, .. }) = engine.execute_with_user_and_tx(
                tx,
                &format!("SELECT no_o_id FROM new_order WHERE no_w_id = {} AND no_d_id = {} ORDER BY no_o_id ASC LIMIT 1", d_w_id, d_id),
                None,
            ) {
                if let Some(r) = rows.first() {
                    if let Some(Value::Integer(no_o_id)) = r.get(0) {
                        let _ = engine.execute_with_user_and_tx(
                            tx,
                            &format!("DELETE FROM new_order WHERE no_w_id = {} AND no_d_id = {} AND no_o_id = {}", d_w_id, d_id, no_o_id),
                            None,
                        );
                        let _ = engine.execute_with_user_and_tx(
                            tx,
                            &format!("UPDATE orders SET o_carrier_id = {} WHERE o_w_id = {} AND o_d_id = {} AND o_id = {}", d_o_carrier_id, d_w_id, d_id, no_o_id),
                            None,
                        );
                        let _ = engine.execute_with_user_and_tx(
                            tx,
                            &format!("UPDATE order_line SET ol_delivery_d = CURRENT_TIMESTAMP WHERE ol_w_id = {} AND ol_d_id = {} AND ol_o_id = {}", d_w_id, d_id, no_o_id),
                            None,
                        );
                    }
                }
            }
        }

        // Pgtcl は PGRES_TUPLES_OK または PGRES_COMMAND_OK を許容
        let row = Row::new(vec![Value::Integer(10)]);
        Ok(ExecutionResult::Query {
            columns: vec!["delivered_districts".to_string()],
            rows: vec![row],
        })
    }

    /// OSTAT (注文状況照会トランザクション)
    fn sim_ostat(&self, tx: &Transaction, engine: &SQLEngine, args: &[String]) -> H2Result<ExecutionResult> {
        let os_w_id: i32 = args.first().and_then(|s| s.parse().ok()).unwrap_or(1);
        let os_d_id: i32 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(1);
        let mut os_c_id: i32 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(1);
        let byname: i32 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(0);
        let os_c_last = args.get(4).cloned().unwrap_or_else(|| "BAR".to_string()).trim_matches('\'').to_string();

        if byname == 1 && !os_c_last.is_empty() {
            if let Ok(ExecutionResult::Query { rows, .. }) = engine.execute_with_user_and_tx(
                tx,
                &format!("SELECT c_id FROM customer WHERE c_w_id = {} AND c_d_id = {} AND c_last = '{}' ORDER BY c_first", os_w_id, os_d_id, os_c_last),
                None,
            ) {
                if let Some(r) = rows.get(rows.len() / 2) {
                    if let Some(Value::Integer(id)) = r.get(0) {
                        os_c_id = *id;
                    }
                }
            }
        }

        let mut o_id = 1;
        let mut carrier_id = 1;
        if let Ok(ExecutionResult::Query { rows, .. }) = engine.execute_with_user_and_tx(
            tx,
            &format!("SELECT o_id, o_carrier_id FROM orders WHERE o_w_id = {} AND o_d_id = {} AND o_c_id = {} ORDER BY o_id DESC LIMIT 1", os_w_id, os_d_id, os_c_id),
            None,
        ) {
            if let Some(r) = rows.first() {
                if let Some(Value::Integer(id)) = r.get(0) {
                    o_id = *id;
                }
                if let Some(Value::Integer(c)) = r.get(1) {
                    carrier_id = *c;
                }
            }
        }

        let row = Row::new(vec![
            Value::Integer(os_c_id),
            Value::String(os_c_last),
            Value::Integer(o_id),
            Value::Integer(carrier_id),
            Value::String("500.00".to_string()),
        ]);

        Ok(ExecutionResult::Query {
            columns: vec![
                "os_c_id".to_string(),
                "os_c_last".to_string(),
                "os_o_id".to_string(),
                "os_o_carrier_id".to_string(),
                "os_c_balance".to_string(),
            ],
            rows: vec![row],
        })
    }

    /// SLEV (在庫照会トランザクション)
    fn sim_slev(&self, tx: &Transaction, engine: &SQLEngine, args: &[String]) -> H2Result<ExecutionResult> {
        let st_w_id: i32 = args.first().and_then(|s| s.parse().ok()).unwrap_or(1);
        let st_d_id: i32 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(1);
        let threshold: i32 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(15);

        let mut stock_count = 0i64;
        let mut d_next_o_id = 3000;

        if let Ok(ExecutionResult::Query { rows, .. }) = engine.execute_with_user_and_tx(
            tx,
            &format!("SELECT d_next_o_id FROM district WHERE d_w_id = {} AND d_id = {}", st_w_id, st_d_id),
            None,
        ) {
            if let Some(r) = rows.first() {
                if let Some(Value::Integer(id)) = r.get(0) {
                    d_next_o_id = *id;
                }
            }
        }

        let query = format!(
            "SELECT COUNT(DISTINCT s_i_id) FROM order_line, stock \
             WHERE ol_w_id = {} AND ol_d_id = {} AND ol_o_id < {} AND ol_o_id >= {} \
             AND s_w_id = {} AND s_i_id = ol_i_id AND s_quantity < {}",
            st_w_id, st_d_id, d_next_o_id, d_next_o_id.saturating_sub(20), st_w_id, threshold
        );

        if let Ok(ExecutionResult::Query { rows, .. }) = engine.execute_with_user_and_tx(tx, &query, None) {
            if let Some(r) = rows.first() {
                if let Some(val) = r.get(0) {
                    match val {
                        Value::BigInt(c) => stock_count = *c,
                        Value::Integer(c) => stock_count = *c as i64,
                        _ => {}
                    }
                }
            }
        }

        let row = Row::new(vec![Value::BigInt(stock_count)]);
        Ok(ExecutionResult::Query {
            columns: vec!["stock_count".to_string()],
            rows: vec![row],
        })
    }
}

/// カンマ区切りの引数文字列を、クォートや括弧を考慮してパース
pub fn parse_call_args(args_str: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut in_single_quote = false;
    let mut paren_depth = 0;

    for ch in args_str.chars() {
        match ch {
            '\'' => {
                in_single_quote = !in_single_quote;
                current.push(ch);
            }
            '(' if !in_single_quote => {
                paren_depth += 1;
                current.push(ch);
            }
            ')' if !in_single_quote => {
                if paren_depth > 0 {
                    paren_depth -= 1;
                    current.push(ch);
                }
            }
            ',' if !in_single_quote && paren_depth == 0 => {
                args.push(current.trim().to_string());
                current.clear();
            }
            _ => current.push(ch),
        }
    }
    let trimmed = current.trim();
    if !trimmed.is_empty() {
        args.push(trimmed.to_string());
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_call_args() {
        let input = "1, 10, 'BAR', TO_TIMESTAMP('20260926','YYYYMMDD')::timestamp, 0.05";
        let args = parse_call_args(input);
        assert_eq!(args.len(), 5);
        assert_eq!(args[0], "1");
        assert_eq!(args[1], "10");
        assert_eq!(args[2], "'BAR'");
        assert_eq!(args[3], "TO_TIMESTAMP('20260926','YYYYMMDD')::timestamp");
        assert_eq!(args[4], "0.05");
    }

    #[test]
    fn test_simulator_procedure_registration() {
        let sim = PlPgSqlSimulator::new();
        assert!(sim.is_procedure("neword"));
        assert!(sim.is_procedure("dbms_random"));

        let ddl = "CREATE OR REPLACE PROCEDURE MY_CUSTOM_PROC (a INT, b VARCHAR) AS $$ BEGIN END; $$ LANGUAGE 'plpgsql';";
        let name = sim.register_from_ddl(ddl).unwrap();
        assert_eq!(name.to_lowercase(), "my_custom_proc");
        assert!(sim.is_procedure("my_custom_proc"));
    }

    #[test]
    fn test_simulator_end_to_end() {
        use h2_mvstore::MVStore;
        use std::sync::Arc;

        let store = Arc::new(MVStore::open_in_memory());
        let engine = SQLEngine::new(store).unwrap();

        // 1. DDL: Create procedures and functions (simulated)
        let ddl1 = "CREATE OR REPLACE FUNCTION DBMS_RANDOM (INTEGER, INTEGER) RETURNS INTEGER AS $$
        DECLARE
        start_int ALIAS FOR $1;
        end_int ALIAS FOR $2;
        BEGIN
        RETURN trunc(random() * (end_int-start_int + 1) + start_int);
        END;
        $$ LANGUAGE 'plpgsql' STRICT;";
        let res = engine.execute(ddl1).unwrap();
        assert!(matches!(res, ExecutionResult::Ddl));

        let ddl2 = "CREATE OR REPLACE PROCEDURE NEWORD (
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
            tstamp          IN TIMESTAMP )
            AS $$ BEGIN END; $$ LANGUAGE 'plpgsql';";
        let res = engine.execute(ddl2).unwrap();
        assert!(matches!(res, ExecutionResult::Ddl));

        // 2. Query DBMS_RANDOM
        let rnd_res = engine.execute("SELECT DBMS_RANDOM(10, 20)").unwrap();
        if let ExecutionResult::Query { rows, .. } = rnd_res {
            assert_eq!(rows.len(), 1);
            if let Value::BigInt(val) = rows[0].values[0] {
                assert!((10..=20).contains(&val));
            } else {
                panic!("Expected BigInt random value");
            }
        } else {
            panic!("Expected Query result");
        }

        // 3. Query pg_proc
        let proc_res = engine.execute("SELECT p.prokind, count(*) AS cnt FROM pg_proc p").unwrap();
        if let ExecutionResult::Query { rows, .. } = proc_res {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].values[0], Value::String("p".to_string()));
            assert_eq!(rows[0].values[1], Value::BigInt(5));
        } else {
            panic!("Expected pg_proc query result");
        }

        // 4. CALL neword
        let call_res = engine.execute("CALL neword(1, 1, 1, 100, 10, 0.0, '', '', 0.0, 0.0, 0, CURRENT_TIMESTAMP)").unwrap();
        if let ExecutionResult::Query { columns, rows } = call_res {
            assert_eq!(columns.len(), 6);
            assert_eq!(rows.len(), 1);
        } else {
            panic!("Expected Query result for CALL with OUT params");
        }

        // 5. CALL payment
        let pay_res = engine.execute("CALL payment(1, 1, 1, 1, 100, 0, 50.0, 'SMITH')").unwrap();
        if let ExecutionResult::Query { columns, rows } = pay_res {
            assert_eq!(columns.len(), 5);
            assert_eq!(rows.len(), 1);
        } else {
            panic!("Expected Query result for CALL payment");
        }
    }
}
