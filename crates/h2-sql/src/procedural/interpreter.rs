use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use h2_mvstore::Transaction;
use h2_types::{DataType, H2Error, H2Result, Value};
use crate::catalog::Catalog;
use crate::executor::{ExecutionResult, SQLEngine};
use crate::procedural::ast::*;
use crate::row::Row;

static RNG_SEED: AtomicU64 = AtomicU64::new(0x9E3779B97F4A7C15);

fn next_random_f64() -> f64 {
    let mut x = RNG_SEED.load(Ordering::Relaxed);
    if x == 0 {
        x = Instant::now().elapsed().as_nanos() as u64 ^ 0x5DEECE66D;
    }
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    RNG_SEED.store(x, Ordering::Relaxed);
    (x as f64) / (u64::MAX as f64)
}

/// 変数環境（スコープスタック）
#[derive(Debug, Clone, Default)]
pub struct ProcEnv {
    scopes: Vec<HashMap<String, Value>>,
}

impl ProcEnv {
    pub fn new() -> Self {
        Self {
            scopes: vec![HashMap::new()],
        }
    }

    pub fn push_scope(&mut self) {
        self.scopes.push(HashMap::new());
    }

    pub fn pop_scope(&mut self) {
        if self.scopes.len() > 1 {
            self.scopes.pop();
        }
    }

    pub fn define(&mut self, name: &str, val: Value) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.insert(name.to_lowercase(), val);
        }
    }

    pub fn set(&mut self, name: &str, val: Value) {
        let lower = name.to_lowercase();
        for scope in self.scopes.iter_mut().rev() {
            if scope.contains_key(&lower) {
                scope.insert(lower, val);
                return;
            }
        }
        // 見つからなければ最上位スコープに定義
        self.define(name, val);
    }

    pub fn get(&self, name: &str) -> Option<Value> {
        let lower = name.to_lowercase();
        for scope in self.scopes.iter().rev() {
            if let Some(v) = scope.get(&lower) {
                return Some(v.clone());
            }
        }
        None
    }

    pub fn all_variables(&self) -> HashMap<String, Value> {
        let mut map = HashMap::new();
        for scope in &self.scopes {
            for (k, v) in scope {
                map.insert(k.clone(), v.clone());
            }
        }
        map
    }
}

/// 制御フローステータス
#[derive(Debug, Clone)]
pub enum ControlFlow {
    None,
    Return(Option<Value>),
    Exit(Option<String>),
    Continue(Option<String>),
}

/// 手続き実行インタプリタ
pub struct ProcInterpreter<'a> {
    pub catalog: Option<Arc<Catalog>>,
    pub engine: Option<&'a SQLEngine>,
    pub tx: Option<&'a Transaction>,
    pub caller_user: Option<String>,
}

impl<'a> ProcInterpreter<'a> {
    pub fn new(
        catalog: Option<Arc<Catalog>>,
        engine: Option<&'a SQLEngine>,
        tx: Option<&'a Transaction>,
        caller_user: Option<String>,
    ) -> Self {
        Self {
            catalog,
            engine,
            tx,
            caller_user,
        }
    }

    /// ルーチン（関数またはプロシージャ）の実行
    pub fn execute_routine(
        &mut self,
        routine: &RoutineDef,
        args: &[Value],
    ) -> H2Result<ExecutionResult> {
        let mut env = ProcEnv::new();

        // 1. 引数のバインド
        for (i, arg) in args.iter().enumerate() {
            let pos_name = format!("${}", i + 1);
            env.define(&pos_name, arg.clone());
            if let Some(param) = routine.parameters.get(i) {
                env.define(&param.name, arg.clone());
            }
        }

        // 2. 本文ブロックの実行
        let flow = self.execute_block(&routine.body, &mut env)?;

        // 3. 結果の生成
        match flow {
            ControlFlow::Return(Some(val)) => {
                let col_name = routine.name.clone();
                let row = Row::new(vec![val]);
                Ok(ExecutionResult::Query {
                    columns: vec![col_name],
                    rows: vec![row],
                })
            }
            ControlFlow::Return(None) | ControlFlow::None => {
                // OUT 引数の収集
                let mut out_cols = Vec::new();
                let mut out_vals = Vec::new();
                for param in &routine.parameters {
                    if param.mode == ParamMode::Out || param.mode == ParamMode::InOut {
                        out_cols.push(param.name.clone());
                        let val = env.get(&param.name).unwrap_or(Value::Null);
                        out_vals.push(val);
                    }
                }

                if !out_cols.is_empty() {
                    let row = Row::new(out_vals);
                    Ok(ExecutionResult::Query {
                        columns: out_cols,
                        rows: vec![row],
                    })
                } else {
                    Ok(ExecutionResult::Ddl)
                }
            }
            ControlFlow::Exit(_) | ControlFlow::Continue(_) => {
                Ok(ExecutionResult::Ddl)
            }
        }
    }

    /// ブロック実行
    pub fn execute_block(
        &mut self,
        block: &ProcBlock,
        env: &mut ProcEnv,
    ) -> H2Result<ControlFlow> {
        env.push_scope();

        // 変数宣言の初期化
        for decl in &block.declarations {
            let init_val = if let Some(alias_pos) = decl.alias_for_pos {
                let pos_name = format!("${}", alias_pos);
                env.get(&pos_name).unwrap_or(Value::Null)
            } else if let Some(ref def_expr) = decl.default {
                self.evaluate_expr(def_expr, env)?
            } else {
                Value::Null
            };
            let coerced = coerce_type(init_val, &decl.data_type);
            env.define(&decl.name, coerced);
        }

        let mut final_flow = ControlFlow::None;
        let mut exec_error = None;

        for stmt in &block.statements {
            match self.execute_stmt(stmt, env) {
                Ok(ControlFlow::None) => {}
                Ok(flow) => {
                    final_flow = flow;
                    break;
                }
                Err(err) => {
                    exec_error = Some(err);
                    break;
                }
            }
        }

        // EXCEPTION ハンドラの処理
        if let Some(err) = exec_error {
            if !block.exception_handlers.is_empty() {
                let mut handled = false;
                for handler in &block.exception_handlers {
                    if handler.condition.eq_ignore_ascii_case("OTHERS") {
                        for h_stmt in &handler.statements {
                            let f = self.execute_stmt(h_stmt, env)?;
                            if !matches!(f, ControlFlow::None) {
                                final_flow = f;
                                break;
                            }
                        }
                        handled = true;
                        break;
                    }
                }
                if !handled {
                    env.pop_scope();
                    return Err(err);
                }
            } else {
                env.pop_scope();
                return Err(err);
            }
        }

        env.pop_scope();
        Ok(final_flow)
    }

    /// 各種文の実行
    pub fn execute_stmt(
        &mut self,
        stmt: &ProcStmt,
        env: &mut ProcEnv,
    ) -> H2Result<ControlFlow> {
        match stmt {
            ProcStmt::Null => Ok(ControlFlow::None),
            ProcStmt::Assign { target, expr } => {
                let val = self.evaluate_expr(expr, env)?;
                env.set(target, val);
                Ok(ControlFlow::None)
            }
            ProcStmt::If { branches, else_branch } => {
                for (cond, body) in branches {
                    let cond_val = self.evaluate_expr(cond, env)?;
                    if is_truthy(&cond_val) {
                        for s in body {
                            let flow = self.execute_stmt(s, env)?;
                            if !matches!(flow, ControlFlow::None) {
                                return Ok(flow);
                            }
                        }
                        return Ok(ControlFlow::None);
                    }
                }
                if let Some(else_stmts) = else_branch {
                    for s in else_stmts {
                        let flow = self.execute_stmt(s, env)?;
                        if !matches!(flow, ControlFlow::None) {
                            return Ok(flow);
                        }
                    }
                }
                Ok(ControlFlow::None)
            }
            ProcStmt::While { condition, body, label: _ } => {
                while is_truthy(&self.evaluate_expr(condition, env)?) {
                    let mut broken = false;
                    for s in body {
                        match self.execute_stmt(s, env)? {
                            ControlFlow::None => {}
                            ControlFlow::Return(v) => return Ok(ControlFlow::Return(v)),
                            ControlFlow::Exit(_) => {
                                broken = true;
                                break;
                            }
                            ControlFlow::Continue(_) => {
                                break;
                            }
                        }
                    }
                    if broken {
                        break;
                    }
                }
                Ok(ControlFlow::None)
            }
            ProcStmt::ForRange {
                var_name,
                start,
                end,
                step,
                reverse,
                body,
            } => {
                let s_val = self.evaluate_expr(start, env)?.to_i64().unwrap_or(0);
                let e_val = self.evaluate_expr(end, env)?.to_i64().unwrap_or(0);
                let step_val = if let Some(st) = step {
                    self.evaluate_expr(st, env)?.to_i64().unwrap_or(1).max(1)
                } else {
                    1
                };

                let mut current = if *reverse { e_val } else { s_val };
                let target = if *reverse { s_val } else { e_val };

                loop {
                    let condition = if *reverse {
                        current >= target
                    } else {
                        current <= target
                    };
                    if !condition {
                        break;
                    }

                    env.set(var_name, Value::BigInt(current));

                    let mut broken = false;
                    for s in body {
                        match self.execute_stmt(s, env)? {
                            ControlFlow::None => {}
                            ControlFlow::Return(v) => return Ok(ControlFlow::Return(v)),
                            ControlFlow::Exit(_) => {
                                broken = true;
                                break;
                            }
                            ControlFlow::Continue(_) => {
                                break;
                            }
                        }
                    }
                    if broken {
                        break;
                    }

                    if *reverse {
                        current -= step_val;
                    } else {
                        current += step_val;
                    }
                }
                Ok(ControlFlow::None)
            }
            ProcStmt::Loop { body, label: _ } => {
                loop {
                    let mut broken = false;
                    for s in body {
                        match self.execute_stmt(s, env)? {
                            ControlFlow::None => {}
                            ControlFlow::Return(v) => return Ok(ControlFlow::Return(v)),
                            ControlFlow::Exit(_) => {
                                broken = true;
                                break;
                            }
                            ControlFlow::Continue(_) => {
                                break;
                            }
                        }
                    }
                    if broken {
                        break;
                    }
                }
                Ok(ControlFlow::None)
            }
            ProcStmt::Exit { condition, label } => {
                if let Some(cond) = condition {
                    if is_truthy(&self.evaluate_expr(cond, env)?) {
                        return Ok(ControlFlow::Exit(label.clone()));
                    }
                } else {
                    return Ok(ControlFlow::Exit(label.clone()));
                }
                Ok(ControlFlow::None)
            }
            ProcStmt::Continue { condition, label } => {
                if let Some(cond) = condition {
                    if is_truthy(&self.evaluate_expr(cond, env)?) {
                        return Ok(ControlFlow::Continue(label.clone()));
                    }
                } else {
                    return Ok(ControlFlow::Continue(label.clone()));
                }
                Ok(ControlFlow::None)
            }
            ProcStmt::Return { value } => {
                let ret_val = if let Some(expr) = value {
                    Some(self.evaluate_expr(expr, env)?)
                } else {
                    None
                };
                Ok(ControlFlow::Return(ret_val))
            }
            ProcStmt::ReturnNext { value: _ } => {
                // 将来の集合返却用
                Ok(ControlFlow::None)
            }
            ProcStmt::ReturnQuery { query: _ } => {
                // 将来の集合問合せ返却用
                Ok(ControlFlow::None)
            }
            ProcStmt::Raise { level, message, params } => {
                let mut formatted = message.clone();
                for param in params {
                    let val = self.evaluate_expr(param, env)?;
                    if let Some(pos) = formatted.find('%') {
                        formatted.replace_range(pos..pos + 1, &val.to_string());
                    }
                }

                match level {
                    RaiseLevel::Exception => {
                        Err(H2Error::Execution(format!("PL/pgSQL Exception: {}", formatted)))
                    }
                    RaiseLevel::Warning => {
                        tracing::warn!("PL/pgSQL Warning: {}", formatted);
                        Ok(ControlFlow::None)
                    }
                    RaiseLevel::Notice | RaiseLevel::Info => {
                        tracing::info!("PL/pgSQL Notice: {}", formatted);
                        Ok(ControlFlow::None)
                    }
                }
            }
            ProcStmt::Perform { expr } => {
                let _ = self.evaluate_expr(expr, env)?;
                Ok(ControlFlow::None)
            }
            ProcStmt::SelectInto { targets, query, strict } => {
                let vars = env.all_variables();
                let bound_sql = substitute_vars_in_sql(query, &vars);

                let engine = self.engine.ok_or_else(|| {
                    H2Error::Execution("SQLEngine context required for SELECT INTO".to_string())
                })?;
                let tx = self.tx.ok_or_else(|| {
                    H2Error::Execution("Transaction context required for SELECT INTO".to_string())
                })?;

                let res = engine.execute_with_user_and_tx(tx, &bound_sql, self.caller_user.as_deref())?;
                if let ExecutionResult::Query { rows, .. } = res {
                    if *strict && rows.is_empty() {
                        return Err(H2Error::Execution("query returned no rows for SELECT INTO STRICT".to_string()));
                    }
                    if *strict && rows.len() > 1 {
                        return Err(H2Error::Execution("query returned more than one row for SELECT INTO STRICT".to_string()));
                    }
                    if let Some(row) = rows.first() {
                        for (idx, target_name) in targets.iter().enumerate() {
                            if let Some(val) = row.values.get(idx) {
                                env.set(target_name, val.clone());
                            }
                        }
                    }
                }
                Ok(ControlFlow::None)
            }
            ProcStmt::ExecuteDynamic { query_expr, into_targets, using_params } => {
                let query_val = self.evaluate_expr(query_expr, env)?;
                let query_str = query_val.to_string();

                let mut bound_sql = query_str;
                for (i, p) in using_params.iter().enumerate() {
                    let v = self.evaluate_expr(p, env)?;
                    let placeholder = format!("${}", i + 1);
                    bound_sql = bound_sql.replace(&placeholder, &value_to_sql_literal(&v));
                }

                let engine = self.engine.ok_or_else(|| {
                    H2Error::Execution("SQLEngine context required for EXECUTE".to_string())
                })?;
                let tx = self.tx.ok_or_else(|| {
                    H2Error::Execution("Transaction context required for EXECUTE".to_string())
                })?;

                let res = engine.execute_with_user_and_tx(tx, &bound_sql, self.caller_user.as_deref())?;
                if !into_targets.is_empty() {
                    if let ExecutionResult::Query { rows, .. } = res {
                        if let Some(row) = rows.first() {
                            for (idx, target_name) in into_targets.iter().enumerate() {
                                if let Some(val) = row.values.get(idx) {
                                    env.set(target_name, val.clone());
                                }
                            }
                        }
                    }
                }
                Ok(ControlFlow::None)
            }
            ProcStmt::SqlStmt { sql } => {
                let vars = env.all_variables();
                let bound_sql = substitute_vars_in_sql(sql, &vars);

                let engine = self.engine.ok_or_else(|| {
                    H2Error::Execution("SQLEngine context required for SQL execution".to_string())
                })?;
                let tx = self.tx.ok_or_else(|| {
                    H2Error::Execution("Transaction context required for SQL execution".to_string())
                })?;

                engine.execute_with_user_and_tx(tx, &bound_sql, self.caller_user.as_deref())?;
                Ok(ControlFlow::None)
            }
            ProcStmt::Block(sub_block) => {
                self.execute_block(sub_block, env)
            }
        }
    }

    /// 式の評価
    pub fn evaluate_expr(
        &mut self,
        expr: &ProcExpr,
        env: &mut ProcEnv,
    ) -> H2Result<Value> {
        match expr {
            ProcExpr::Literal(val) => Ok(val.clone()),
            ProcExpr::Variable(name) => {
                Ok(env.get(name).unwrap_or(Value::Null))
            }
            ProcExpr::PositionalArg(idx) => {
                let pos_name = format!("${}", idx);
                Ok(env.get(&pos_name).unwrap_or(Value::Null))
            }
            ProcExpr::IsNull(inner) => {
                let val = self.evaluate_expr(inner, env)?;
                Ok(Value::Boolean(val.is_null()))
            }
            ProcExpr::IsNotNull(inner) => {
                let val = self.evaluate_expr(inner, env)?;
                Ok(Value::Boolean(!val.is_null()))
            }
            ProcExpr::Unary { op, expr: inner } => {
                let val = self.evaluate_expr(inner, env)?;
                match op {
                    ProcUnaryOp::Not => {
                        if val.is_null() {
                            Ok(Value::Null)
                        } else {
                            Ok(Value::Boolean(!is_truthy(&val)))
                        }
                    }
                    ProcUnaryOp::Neg => match val {
                        Value::Integer(i) => Ok(Value::Integer(-i)),
                        Value::BigInt(i) => Ok(Value::BigInt(-i)),
                        Value::Float(f) => Ok(Value::Float(-f)),
                        Value::Double(d) => Ok(Value::Double(-d)),
                        Value::Decimal(d) => Ok(Value::Decimal(-d)),
                        Value::Null => Ok(Value::Null),
                        _ => Err(H2Error::TypeError("Cannot negate non-numeric value".to_string())),
                    },
                }
            }
            ProcExpr::Binary { left, op, right } => {
                // AND / OR の短絡評価
                if *op == ProcBinaryOp::And {
                    let l_val = self.evaluate_expr(left, env)?;
                    if !is_truthy(&l_val) {
                        return Ok(Value::Boolean(false));
                    }
                    let r_val = self.evaluate_expr(right, env)?;
                    return Ok(Value::Boolean(is_truthy(&r_val)));
                } else if *op == ProcBinaryOp::Or {
                    let l_val = self.evaluate_expr(left, env)?;
                    if is_truthy(&l_val) {
                        return Ok(Value::Boolean(true));
                    }
                    let r_val = self.evaluate_expr(right, env)?;
                    return Ok(Value::Boolean(is_truthy(&r_val)));
                }

                let l_val = self.evaluate_expr(left, env)?;
                let r_val = self.evaluate_expr(right, env)?;

                self.eval_binary_op(&l_val, *op, &r_val)
            }
            ProcExpr::FunctionCall { name, args } => {
                let upper_name = name.to_uppercase();
                let mut arg_vals = Vec::new();
                for a in args {
                    arg_vals.push(self.evaluate_expr(a, env)?);
                }

                // 1. 手続き言語組み込み関数
                match upper_name.as_str() {
                    "RANDOM" => {
                        let r = next_random_f64();
                        return Ok(Value::Double(r));
                    }
                    "TRUNC" | "TRUNCATE" => {
                        if let Some(first) = arg_vals.first() {
                            if let Some(f) = first.to_f64() {
                                return Ok(Value::BigInt(f.trunc() as i64));
                            }
                        }
                        return Ok(Value::Null);
                    }
                    "ROUND" => {
                        if let Some(first) = arg_vals.first() {
                            if let Some(f) = first.to_f64() {
                                return Ok(Value::BigInt(f.round() as i64));
                            }
                        }
                        return Ok(Value::Null);
                    }
                    "ABS" => {
                        if let Some(first) = arg_vals.first() {
                            if let Some(i) = first.to_i64() {
                                return Ok(Value::BigInt(i.abs()));
                            } else if let Some(f) = first.to_f64() {
                                return Ok(Value::Double(f.abs()));
                            }
                        }
                        return Ok(Value::Null);
                    }
                    "COALESCE" => {
                        for a in arg_vals {
                            if !a.is_null() {
                                return Ok(a);
                            }
                        }
                        return Ok(Value::Null);
                    }
                    "LENGTH" => {
                        if let Some(first) = arg_vals.first() {
                            return Ok(Value::Integer(first.to_string().chars().count() as i32));
                        }
                        return Ok(Value::Null);
                    }
                    "LOWER" => {
                        if let Some(first) = arg_vals.first() {
                            return Ok(Value::String(first.to_string().to_lowercase()));
                        }
                        return Ok(Value::Null);
                    }
                    "UPPER" => {
                        if let Some(first) = arg_vals.first() {
                            return Ok(Value::String(first.to_string().to_uppercase()));
                        }
                        return Ok(Value::Null);
                    }
                    _ => {}
                }

                // 2. カタログに登録されたユーザー定義関数
                if let Some(catalog) = &self.catalog {
                    if let Some(def) = catalog.get_routine(name) {
                        if def.kind == RoutineKind::Function {
                            let mut sub_interp = ProcInterpreter::new(
                                Some(Arc::clone(catalog)),
                                self.engine,
                                self.tx,
                                self.caller_user.clone(),
                            );
                            let sub_res = sub_interp.execute_routine(&def, &arg_vals)?;
                            if let ExecutionResult::Query { rows, .. } = sub_res {
                                if let Some(row) = rows.first() {
                                    if let Some(v) = row.values.first() {
                                        return Ok(v.clone());
                                    }
                                }
                            }
                            return Ok(Value::Null);
                        }
                    }
                }

                Err(H2Error::Execution(format!("Unsupported function in expression: {}", name)))
            }
        }
    }

    fn eval_binary_op(&self, left: &Value, op: ProcBinaryOp, right: &Value) -> H2Result<Value> {
        if left.is_null() || right.is_null() {
            return match op {
                ProcBinaryOp::Eq => Ok(Value::Boolean(left.is_null() && right.is_null())),
                ProcBinaryOp::NotEq => Ok(Value::Boolean(!(left.is_null() && right.is_null()))),
                _ => Ok(Value::Null),
            };
        }

        match op {
            ProcBinaryOp::Concat => {
                Ok(Value::String(format!("{}{}", left, right)))
            }
            ProcBinaryOp::Eq => Ok(Value::Boolean(left == right)),
            ProcBinaryOp::NotEq => Ok(Value::Boolean(left != right)),
            ProcBinaryOp::Lt => Ok(Value::Boolean(left < right)),
            ProcBinaryOp::LtEq => Ok(Value::Boolean(left <= right)),
            ProcBinaryOp::Gt => Ok(Value::Boolean(left > right)),
            ProcBinaryOp::GtEq => Ok(Value::Boolean(left >= right)),
            ProcBinaryOp::Like => {
                let l_str = left.to_string();
                let pat = right.to_string().replace('%', ".*").replace('_', ".");
                let re = regex::Regex::new(&format!("^{}$", pat)).unwrap_or_else(|_| regex::Regex::new(".*").unwrap());
                Ok(Value::Boolean(re.is_match(&l_str)))
            }
            ProcBinaryOp::Add => {
                if let (Some(l), Some(r)) = (left.to_i64(), right.to_i64()) {
                    Ok(Value::BigInt(l + r))
                } else if let (Some(l), Some(r)) = (left.to_f64(), right.to_f64()) {
                    Ok(Value::Double(l + r))
                } else {
                    Err(H2Error::TypeError("Add operands must be numeric".to_string()))
                }
            }
            ProcBinaryOp::Sub => {
                if let (Some(l), Some(r)) = (left.to_i64(), right.to_i64()) {
                    Ok(Value::BigInt(l - r))
                } else if let (Some(l), Some(r)) = (left.to_f64(), right.to_f64()) {
                    Ok(Value::Double(l - r))
                } else {
                    Err(H2Error::TypeError("Sub operands must be numeric".to_string()))
                }
            }
            ProcBinaryOp::Mul => {
                if let (Some(l), Some(r)) = (left.to_i64(), right.to_i64()) {
                    Ok(Value::BigInt(l * r))
                } else if let (Some(l), Some(r)) = (left.to_f64(), right.to_f64()) {
                    Ok(Value::Double(l * r))
                } else {
                    Err(H2Error::TypeError("Mul operands must be numeric".to_string()))
                }
            }
            ProcBinaryOp::Div => {
                if let (Some(l), Some(r)) = (left.to_i64(), right.to_i64()) {
                    if r == 0 {
                        return Err(H2Error::Execution("Division by zero".to_string()));
                    }
                    Ok(Value::BigInt(l / r))
                } else if let (Some(l), Some(r)) = (left.to_f64(), right.to_f64()) {
                    if r == 0.0 {
                        return Err(H2Error::Execution("Division by zero".to_string()));
                    }
                    Ok(Value::Double(l / r))
                } else {
                    Err(H2Error::TypeError("Div operands must be numeric".to_string()))
                }
            }
            ProcBinaryOp::Mod => {
                if let (Some(l), Some(r)) = (left.to_i64(), right.to_i64()) {
                    if r == 0 {
                        return Err(H2Error::Execution("Modulo by zero".to_string()));
                    }
                    Ok(Value::BigInt(l % r))
                } else {
                    Err(H2Error::TypeError("Mod operands must be integers".to_string()))
                }
            }
            _ => Ok(Value::Null),
        }
    }
}

fn is_truthy(val: &Value) -> bool {
    match val {
        Value::Boolean(b) => *b,
        Value::Integer(i) => *i != 0,
        Value::BigInt(i) => *i != 0,
        Value::Double(d) => *d != 0.0,
        Value::String(s) => !s.is_empty() && s != "0" && !s.eq_ignore_ascii_case("false"),
        _ => false,
    }
}

fn coerce_type(val: Value, target_type: &DataType) -> Value {
    if val.is_null() {
        return Value::Null;
    }
    match target_type {
        DataType::Integer => Value::Integer(val.to_i64().unwrap_or(0) as i32),
        DataType::BigInt => Value::BigInt(val.to_i64().unwrap_or(0)),
        DataType::SmallInt => Value::SmallInt(val.to_i64().unwrap_or(0) as i16),
        DataType::TinyInt => Value::TinyInt(val.to_i64().unwrap_or(0) as i8),
        DataType::Double => Value::Double(val.to_f64().unwrap_or(0.0)),
        DataType::Float => Value::Float(val.to_f64().unwrap_or(0.0) as f32),
        DataType::Boolean => Value::Boolean(is_truthy(&val)),
        DataType::VarChar(_) | DataType::Char(_) => match val {
            Value::String(s) => Value::String(s),
            other => Value::String(other.to_string().trim_matches('\'').to_string()),
        },
        _ => val,
    }
}

/// SQL 文中の変数をリテラル値に置換（引用符外の単語のみ）
fn substitute_vars_in_sql(sql: &str, vars: &HashMap<String, Value>) -> String {
    if vars.is_empty() {
        return sql.to_string();
    }

    let mut result = String::new();
    let mut in_single_quote = false;
    let mut current_word = String::new();

    for ch in sql.chars() {
        if ch == '\'' {
            in_single_quote = !in_single_quote;
            if !current_word.is_empty() {
                replace_or_append_word(&mut result, &current_word, vars);
                current_word.clear();
            }
            result.push(ch);
        } else if in_single_quote {
            result.push(ch);
        } else if ch.is_alphanumeric() || ch == '_' || ch == '$' {
            current_word.push(ch);
        } else {
            if !current_word.is_empty() {
                replace_or_append_word(&mut result, &current_word, vars);
                current_word.clear();
            }
            result.push(ch);
        }
    }
    if !current_word.is_empty() {
        replace_or_append_word(&mut result, &current_word, vars);
    }

    result
}

fn replace_or_append_word(out: &mut String, word: &str, vars: &HashMap<String, Value>) {
    let lower = word.to_lowercase();
    if let Some(val) = vars.get(&lower) {
        out.push_str(&value_to_sql_literal(val));
    } else {
        out.push_str(word);
    }
}

fn value_to_sql_literal(val: &Value) -> String {
    match val {
        Value::Null => "NULL".to_string(),
        Value::Boolean(b) => if *b { "TRUE".to_string() } else { "FALSE".to_string() },
        Value::Integer(i) => i.to_string(),
        Value::BigInt(i) => i.to_string(),
        Value::SmallInt(i) => i.to_string(),
        Value::TinyInt(i) => i.to_string(),
        Value::Float(f) => f.to_string(),
        Value::Double(d) => d.to_string(),
        Value::Decimal(d) => d.to_string(),
        Value::String(s) => format!("'{}'", s.replace('\'', "''")),
        Value::Date(d) => format!("'{}'", d),
        Value::Timestamp(t) => format!("'{}'", t.format("%Y-%m-%d %H:%M:%S")),
        _ => format!("'{}'", val),
    }
}
