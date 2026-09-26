pub mod ast;
pub mod interpreter;
pub mod plpgsql;

use std::sync::Arc;
use h2_mvstore::Transaction;
use h2_types::{H2Error, H2Result, Value};
use crate::catalog::Catalog;
use crate::executor::{ExecutionResult, SQLEngine};

pub use ast::{ParamDef, ParamMode, ProcBlock, ProcExpr, ProcStmt, RoutineDef, RoutineKind, RoutineLanguage, VarDecl};
pub use interpreter::ProcInterpreter;
pub use plpgsql::parse_create_routine;

/// 手続き型実行エンジン
#[derive(Debug, Default, Clone)]
pub struct ProceduralEngine {}

impl ProceduralEngine {
    pub fn new() -> Self {
        Self {}
    }

    /// DDL からルーチン（関数またはプロシージャ）を構文解析し、カタログへ登録・永続化
    pub fn register_from_ddl(&self, catalog: &Catalog, ddl: &str) -> H2Result<String> {
        let routine_def = parse_create_routine(ddl)?;
        let name = routine_def.name.clone();
        catalog.create_routine(routine_def)?;
        Ok(name)
    }

    /// ルーチンの削除 (DROP FUNCTION / DROP PROCEDURE)
    pub fn drop_routine(&self, catalog: &Catalog, ddl: &str) -> H2Result<()> {
        let trimmed = ddl.trim().trim_end_matches(';').trim();
        let upper = trimmed.to_uppercase();

        let if_exists = upper.contains("IF EXISTS");
        let kw = if upper.starts_with("DROP PROCEDURE") {
            "DROP PROCEDURE"
        } else if upper.starts_with("DROP FUNCTION") {
            "DROP FUNCTION"
        } else {
            return Err(H2Error::SqlParse("Expected DROP FUNCTION or DROP PROCEDURE".to_string()));
        };

        let mut after = trimmed[kw.len()..].trim();
        if if_exists {
            let if_idx = after.to_uppercase().find("IF EXISTS").unwrap();
            after = after[if_idx + "IF EXISTS".len()..].trim();
        }

        // 引数部 `(int, ...)` やクォートを取り除く
        let clean_name = after.split('(').next().unwrap_or(after).trim();
        let proc_name = clean_name.trim_matches('"').trim_matches('\'').trim();

        if proc_name.is_empty() {
            return Err(H2Error::SqlParse("Missing routine name in DROP".to_string()));
        }

        catalog.drop_routine(proc_name, if_exists)
    }

    /// CALL 文のディスパッチ
    pub fn execute_call(
        &self,
        catalog: &Arc<Catalog>,
        engine: &SQLEngine,
        tx: &Transaction,
        caller_user: Option<&str>,
        sql: &str,
    ) -> H2Result<ExecutionResult> {
        let trimmed = sql.trim().trim_end_matches(';').trim();
        if !trimmed.to_uppercase().starts_with("CALL ") {
            return Err(H2Error::Execution(format!("Invalid CALL syntax: {}", sql)));
        }
        let after_call = trimmed[4..].trim(); // "proc_name(...)"

        let open_paren = after_call.find('(').ok_or_else(|| {
            H2Error::Execution(format!("Invalid CALL syntax, missing '(': {}", sql))
        })?;
        let proc_name = after_call[..open_paren].trim().to_lowercase();
        let close_paren = after_call.rfind(')').unwrap_or(after_call.len());
        let args_str = &after_call[open_paren + 1..close_paren];
        let arg_strings = parse_call_args(args_str);

        // カタログから登録済みルーチンを検索
        let routine_def = catalog.get_routine(&proc_name).ok_or_else(|| {
            H2Error::Execution(format!("Procedure '{}' does not exist", proc_name))
        })?;

        // 引数文字列を Value に変換
        let mut arg_values = Vec::new();
        for s in &arg_strings {
            let s_trimmed = s.trim();
            if s_trimmed.eq_ignore_ascii_case("null") {
                arg_values.push(Value::Null);
            } else if s_trimmed.eq_ignore_ascii_case("true") {
                arg_values.push(Value::Boolean(true));
            } else if s_trimmed.eq_ignore_ascii_case("false") {
                arg_values.push(Value::Boolean(false));
            } else if s_trimmed.eq_ignore_ascii_case("current_timestamp")
                || s_trimmed.eq_ignore_ascii_case("now()")
            {
                arg_values.push(Value::Timestamp(chrono::Utc::now()));
            } else if (s_trimmed.starts_with('\'') && s_trimmed.ends_with('\''))
                || (s_trimmed.starts_with('"') && s_trimmed.ends_with('"'))
            {
                let clean = &s_trimmed[1..s_trimmed.len().saturating_sub(1)];
                arg_values.push(Value::String(clean.to_string()));
            } else if let Ok(i) = s_trimmed.parse::<i64>() {
                arg_values.push(Value::BigInt(i));
            } else if let Ok(f) = s_trimmed.parse::<f64>() {
                arg_values.push(Value::Double(f));
            } else {
                arg_values.push(Value::String(s_trimmed.to_string()));
            }
        }

        // 3. インタプリタで実行
        let mut interp = ProcInterpreter::new(
            Some(Arc::clone(catalog)),
            Some(engine),
            Some(tx),
            caller_user.map(|s| s.to_string()),
        );

        interp.execute_routine(&routine_def, &arg_values)
    }

    /// スカラーユーザー定義関数の評価
    pub fn execute_function(
        &self,
        catalog: &Arc<Catalog>,
        engine: &SQLEngine,
        tx: &Transaction,
        caller_user: Option<&str>,
        func_name: &str,
        args: &[Value],
    ) -> H2Result<Value> {
        let routine_def = catalog.get_routine(func_name).ok_or_else(|| {
            H2Error::Execution(format!("Function '{}' does not exist", func_name))
        })?;

        let mut interp = ProcInterpreter::new(
            Some(Arc::clone(catalog)),
            Some(engine),
            Some(tx),
            caller_user.map(|s| s.to_string()),
        );

        let res = interp.execute_routine(&routine_def, args)?;
        if let ExecutionResult::Query { rows, .. } = res {
            if let Some(row) = rows.first() {
                if let Some(v) = row.values.first() {
                    return Ok(v.clone());
                }
            }
        }
        Ok(Value::Null)
    }
}

fn parse_call_args(args_str: &str) -> Vec<String> {
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
                paren_depth -= 1;
                current.push(ch);
            }
            ',' if !in_single_quote && paren_depth == 0 => {
                args.push(current.trim().to_string());
                current.clear();
            }
            _ => {
                current.push(ch);
            }
        }
    }
    if !current.trim().is_empty() {
        args.push(current.trim().to_string());
    }
    args
}
