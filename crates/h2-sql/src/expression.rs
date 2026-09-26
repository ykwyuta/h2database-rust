use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use sqlparser::ast::{BinaryOperator, Expr as SqlExpr, FunctionArg, FunctionArgExpr, Value as SqlValue, TrimWhereField};
use std::str::FromStr;
use std::sync::Arc;
use chrono::Datelike;

use h2_types::{H2Error, H2Result, Value, IntervalValue};
use crate::catalog::{Catalog, TableDef};
use crate::executor::ExecutionResult;
use crate::fts::tokenizer::{get_tokenizer, TokenizerKind};
use crate::procedural::{ProcInterpreter, RoutineKind};
use crate::row::Row;

#[derive(Debug, Clone)]
pub struct ColumnBinding {
    pub table_name: Option<String>,
    pub table_alias: Option<String>,
    pub column_name: String,
    pub index: usize,
}

#[derive(Debug, Clone, Default)]
pub struct RowContext {
    pub columns: Vec<ColumnBinding>,
    pub catalog: Option<Arc<Catalog>>,
    pub dialect_mode: Option<h2_types::SqlDialectMode>,
}

impl RowContext {
    pub fn new() -> Self {
        Self { columns: Vec::new(), catalog: None, dialect_mode: None }
    }

    pub fn with_catalog(catalog: Arc<Catalog>) -> Self {
        Self { columns: Vec::new(), catalog: Some(catalog), dialect_mode: None }
    }

    pub fn with_catalog_and_mode(catalog: Arc<Catalog>, mode: h2_types::SqlDialectMode) -> Self {
        Self { columns: Vec::new(), catalog: Some(catalog), dialect_mode: Some(mode) }
    }

    pub fn from_table_def(table: &TableDef, alias: Option<&str>) -> Self {
        let columns = table.columns.iter().enumerate().map(|(i, col)| ColumnBinding {
            table_name: Some(table.name.clone()),
            table_alias: alias.map(|s| s.to_string()),
            column_name: col.name.clone(),
            index: i,
        }).collect();
        Self { columns, catalog: None, dialect_mode: None }
    }

    pub fn append_table(&mut self, table: &TableDef, alias: Option<&str>, base_idx: usize) {
        for (i, col) in table.columns.iter().enumerate() {
            self.columns.push(ColumnBinding {
                table_name: Some(table.name.clone()),
                table_alias: alias.map(|s| s.to_string()),
                column_name: col.name.clone(),
                index: base_idx + i,
            });
        }
    }

    pub fn resolve_column(&self, table_or_alias: Option<&str>, col_name: &str) -> Option<usize> {
        for binding in &self.columns {
            if binding.column_name.eq_ignore_ascii_case(col_name) {
                if let Some(t) = table_or_alias {
                    if binding.table_alias.as_deref().map(|a| a.eq_ignore_ascii_case(t)).unwrap_or(false)
                        || binding.table_name.as_deref().map(|n| n.eq_ignore_ascii_case(t)).unwrap_or(false)
                    {
                        return Some(binding.index);
                    }
                } else {
                    return Some(binding.index);
                }
            }
        }
        None
    }
}

pub fn evaluate_expr(expr: &SqlExpr, table: &TableDef, row: &Row) -> H2Result<Value> {
    let ctx = RowContext::from_table_def(table, None);
    evaluate_expr_context(expr, &ctx, row)
}

pub fn evaluate_expr_context(expr: &SqlExpr, ctx: &RowContext, row: &Row) -> H2Result<Value> {
    match expr {
        SqlExpr::Value(val) => {
            if let SqlValue::SingleQuotedString(s) | SqlValue::DoubleQuotedString(s) = val {
                if s.is_empty() && ctx.dialect_mode.map(|m| m.empty_strings_are_null()).unwrap_or(false) {
                    return Ok(Value::Null);
                }
            }
            evaluate_sql_value(val)
        }
        SqlExpr::Nested(inner) => evaluate_expr_context(inner, ctx, row),
        SqlExpr::IsNull(inner) => {
            let v = evaluate_expr_context(inner, ctx, row)?;
            let is_null = match &v {
                Value::Null => true,
                Value::String(s) if s.is_empty() && ctx.dialect_mode.map(|m| m.empty_strings_are_null()).unwrap_or(false) => true,
                _ => false,
            };
            Ok(Value::Boolean(is_null))
        }
        SqlExpr::IsNotNull(inner) => {
            let v = evaluate_expr_context(inner, ctx, row)?;
            let is_null = match &v {
                Value::Null => true,
                Value::String(s) if s.is_empty() && ctx.dialect_mode.map(|m| m.empty_strings_are_null()).unwrap_or(false) => true,
                _ => false,
            };
            Ok(Value::Boolean(!is_null))
        }
        SqlExpr::UnaryOp { op, expr } => {
            let inner = evaluate_expr_context(expr, ctx, row)?;
            match op {
                sqlparser::ast::UnaryOperator::Minus => match inner {
                    Value::TinyInt(n) => Ok(Value::TinyInt(-n)),
                    Value::SmallInt(n) => Ok(Value::SmallInt(-n)),
                    Value::Integer(n) => Ok(Value::Integer(-n)),
                    Value::BigInt(n) => Ok(Value::BigInt(-n)),
                    Value::Float(f) => Ok(Value::Float(-f)),
                    Value::Double(d) => Ok(Value::Double(-d)),
                    Value::Decimal(d) => Ok(Value::Decimal(-d)),
                    _ => Err(H2Error::TypeError("Cannot negate non-numeric value".to_string())),
                },
                sqlparser::ast::UnaryOperator::Plus => Ok(inner),
                sqlparser::ast::UnaryOperator::Not => match inner {
                    Value::Boolean(b) => Ok(Value::Boolean(!b)),
                    _ => Err(H2Error::TypeError("NOT requires boolean operand".to_string())),
                },
                _ => Err(H2Error::Execution(format!("Unsupported unary operator: {:?}", op))),
            }
        }
        SqlExpr::Identifier(ident) => {
            if let Some(col_idx) = ctx.resolve_column(None, &ident.value) {
                return Ok(row.get(col_idx).cloned().unwrap_or(Value::Null));
            }
            let name_upper = ident.value.to_uppercase();
            match name_upper.as_str() {
                "SYSDATE" => Ok(Value::Timestamp(chrono::Utc::now())),
                "CURRENT_TIMESTAMP" | "NOW" => Ok(Value::Timestamp(chrono::Utc::now())),
                "CURRENT_DATE" | "CURDATE" => Ok(Value::Date(chrono::Utc::now().date_naive())),
                "CURRENT_TIME" | "CURTIME" => Ok(Value::Time(chrono::Utc::now().time())),
                "CURRENT_MODE" => {
                    let mode_str = ctx.dialect_mode.map(|m| m.as_str()).unwrap_or("REGULAR");
                    Ok(Value::String(mode_str.to_string()))
                }
                "USER" | "CURRENT_USER" => Ok(Value::String("sa".to_string())),
                _ => Err(H2Error::Execution(format!("Column '{}' not found", ident.value))),
            }
        }
        SqlExpr::CompoundIdentifier(idents) => {
            if idents.len() == 2 {
                let tbl = &idents[0].value;
                let col = &idents[1].value;
                if col.eq_ignore_ascii_case("NEXTVAL") {
                    if let Some(catalog) = &ctx.catalog {
                        let next_v = catalog.nextval(tbl)?;
                        return Ok(Value::BigInt(next_v));
                    }
                } else if col.eq_ignore_ascii_case("CURRVAL") {
                    if let Some(catalog) = &ctx.catalog {
                        let curr_v = catalog.currval(tbl)?;
                        return Ok(Value::BigInt(curr_v));
                    }
                }
                let col_idx = ctx.resolve_column(Some(tbl), col).ok_or_else(|| {
                    H2Error::Execution(format!("Column '{}.{}' not found", tbl, col))
                })?;
                Ok(row.get(col_idx).cloned().unwrap_or(Value::Null))
            } else {
                Err(H2Error::Execution(format!("Unsupported compound identifier: {:?}", idents)))
            }
        }
        SqlExpr::BinaryOp { left, op, right } => {
            let l_val = evaluate_expr_context(left, ctx, row)?;
            let r_val = evaluate_expr_context(right, ctx, row)?;
            evaluate_binary_op(&l_val, op, &r_val)
        }
        // LIKE 演算子
        SqlExpr::Like { negated, expr, pattern, .. } => {
            let val = evaluate_expr_context(expr, ctx, row)?;
            let pat = evaluate_expr_context(pattern, ctx, row)?;
            let matched = match (&val, &pat) {
                (Value::String(s), Value::String(p)) => {
                    if p.starts_with('%') && p.ends_with('%') {
                        let substr = &p[1..p.len() - 1];
                        s.contains(substr)
                    } else if p.starts_with('%') {
                        s.ends_with(&p[1..])
                    } else if p.ends_with('%') {
                        s.starts_with(&p[..p.len() - 1])
                    } else {
                        s == p
                    }
                }
                _ => false,
            };
            let res = if *negated { !matched } else { matched };
            Ok(Value::Boolean(res))
        }
        // ILIKE 演算子 (PostgreSQL / 大文字小文字無視 LIKE)
        SqlExpr::ILike { negated, expr, pattern, .. } => {
            let val = evaluate_expr_context(expr, ctx, row)?;
            let pat = evaluate_expr_context(pattern, ctx, row)?;
            let matched = match (&val, &pat) {
                (Value::String(s), Value::String(p)) => {
                    let s_lower = s.to_lowercase();
                    let p_lower = p.to_lowercase();
                    if p_lower.starts_with('%') && p_lower.ends_with('%') {
                        let substr = &p_lower[1..p_lower.len() - 1];
                        s_lower.contains(substr)
                    } else if p_lower.starts_with('%') {
                        s_lower.ends_with(&p_lower[1..])
                    } else if p_lower.ends_with('%') {
                        s_lower.starts_with(&p_lower[..p_lower.len() - 1])
                    } else {
                        s_lower == p_lower
                    }
                }
                _ => false,
            };
            let res = if *negated { !matched } else { matched };
            Ok(Value::Boolean(res))
        }
        SqlExpr::InList { expr, list, negated } => {
            let target_val = evaluate_expr_context(expr, ctx, row)?;
            let mut found = false;
            for item in list {
                let item_val = evaluate_expr_context(item, ctx, row)?;
                if target_val == item_val {
                    found = true;
                    break;
                }
            }
            let res = if *negated { !found } else { found };
            Ok(Value::Boolean(res))
        }
        SqlExpr::Between { expr, negated, low, high } => {
            let target_val = evaluate_expr_context(expr, ctx, row)?;
            let low_val = evaluate_expr_context(low, ctx, row)?;
            let high_val = evaluate_expr_context(high, ctx, row)?;
            let in_range = target_val >= low_val && target_val <= high_val;
            let res = if *negated { !in_range } else { in_range };
            Ok(Value::Boolean(res))
        }
        SqlExpr::Case { operand, conditions, results, else_result } => {
            let op_val = if let Some(op) = operand {
                Some(evaluate_expr_context(op, ctx, row)?)
            } else {
                None
            };

            for (cond, res) in conditions.iter().zip(results.iter()) {
                let matched = if let Some(ref base_val) = op_val {
                    let cond_val = evaluate_expr_context(cond, ctx, row)?;
                    base_val == &cond_val
                } else {
                    let cond_val = evaluate_expr_context(cond, ctx, row)?;
                    matches!(cond_val, Value::Boolean(true))
                };

                if matched {
                    return evaluate_expr_context(res, ctx, row);
                }
            }

            if let Some(else_expr) = else_result {
                evaluate_expr_context(else_expr, ctx, row)
            } else {
                Ok(Value::Null)
            }
        }
        SqlExpr::Tuple(exprs) => {
            let mut vals = Vec::with_capacity(exprs.len());
            for e in exprs {
                vals.push(evaluate_expr_context(e, ctx, row)?);
            }
            Ok(Value::Array(vals))
        }
        SqlExpr::Substring { expr, substring_from, substring_for, .. } => {
            let s_val = evaluate_expr_context(expr, ctx, row)?;
            let s = match s_val {
                Value::String(s) => s,
                Value::Null => return Ok(Value::Null),
                _ => s_val.to_string(),
            };
            let from_val = if let Some(f) = substring_from {
                evaluate_expr_context(f, ctx, row)?
            } else {
                Value::Integer(1)
            };
            let for_val = if let Some(l) = substring_for {
                Some(evaluate_expr_context(l, ctx, row)?)
            } else {
                None
            };
            substring_impl(&s, &from_val, for_val.as_ref())
        }
        SqlExpr::Trim { expr, trim_where, trim_what, .. } => {
            let s_val = evaluate_expr_context(expr, ctx, row)?;
            let s = match s_val {
                Value::String(s) => s,
                Value::Null => return Ok(Value::Null),
                _ => s_val.to_string(),
            };
            let chars = if let Some(w) = trim_what {
                let what_val = evaluate_expr_context(w, ctx, row)?;
                match what_val {
                    Value::String(cs) => cs,
                    _ => " ".to_string(),
                }
            } else {
                " ".to_string()
            };
            let trimmed = match trim_where {
                Some(TrimWhereField::Leading) => s.trim_start_matches(|c| chars.contains(c)).to_string(),
                Some(TrimWhereField::Trailing) => s.trim_end_matches(|c| chars.contains(c)).to_string(),
                _ => s.trim_matches(|c| chars.contains(c)).to_string(),
            };
            Ok(Value::String(trimmed))
        }
        SqlExpr::Extract { field, expr, .. } => {
            let val = evaluate_expr_context(expr, ctx, row)?;
            extract_field(&val, &field.to_string())
        }
        SqlExpr::Interval(interval) => {
            let base_val = evaluate_expr_context(&interval.value, ctx, row)?;
            let s = match &base_val {
                Value::String(s) => s.clone(),
                Value::Integer(i) => {
                    if let Some(ref field) = interval.leading_field {
                        format!("{} {:?}", i, field)
                    } else {
                        format!("{} second", i)
                    }
                }
                _ => base_val.to_string(),
            };
            let interval_str = if let Some(ref field) = interval.leading_field {
                if s.split_whitespace().count() == 1 {
                    format!("{} {:?}", s, field)
                } else {
                    s
                }
            } else {
                s
            };
            if let Some(iv) = IntervalValue::parse(&interval_str) {
                Ok(Value::Interval(iv))
            } else {
                Err(H2Error::Execution(format!("Invalid INTERVAL literal: '{}'", interval_str)))
            }
        }
        SqlExpr::Cast { expr, data_type, .. } => {
            let val = evaluate_expr_context(expr, ctx, row)?;
            let target_type = crate::parser::convert_data_type(data_type)?;
            val.cast_to(&target_type)
        }
        SqlExpr::TypedString { data_type, value } => {
            let target_type = crate::parser::convert_data_type(data_type)?;
            Value::String(value.clone()).cast_to(&target_type)
        }
        SqlExpr::Ceil { expr, .. } => {
            let v = evaluate_expr_context(expr, ctx, row)?;
            match v {
                Value::Integer(i) => Ok(Value::Integer(i)),
                Value::BigInt(i) => Ok(Value::BigInt(i)),
                Value::Float(f) => Ok(Value::Float(f.ceil())),
                Value::Double(d) => Ok(Value::Double(d.ceil())),
                Value::Decimal(d) => Ok(Value::Decimal(d.ceil())),
                _ => Err(H2Error::TypeError("CEIL requires numeric argument".to_string())),
            }
        }
        SqlExpr::Floor { expr, .. } => {
            let v = evaluate_expr_context(expr, ctx, row)?;
            match v {
                Value::Integer(i) => Ok(Value::Integer(i)),
                Value::BigInt(i) => Ok(Value::BigInt(i)),
                Value::Float(f) => Ok(Value::Float(f.floor())),
                Value::Double(d) => Ok(Value::Double(d.floor())),
                Value::Decimal(d) => Ok(Value::Decimal(d.floor())),
                _ => Err(H2Error::TypeError("FLOOR requires numeric argument".to_string())),
            }
        }
        // スカラ関数
        SqlExpr::Function(func) => {
            let func_name = func.name.to_string().to_uppercase();
            let args = match &func.args {
                sqlparser::ast::FunctionArguments::List(arg_list) => &arg_list.args,
                sqlparser::ast::FunctionArguments::None => &Vec::new(),
                _ => return Err(H2Error::Execution("Invalid function args".to_string())),
            };

            if func_name == "FT_SEARCH" || func_name == "FT_SEARCH_MORPH" {
                let kind = if func_name == "FT_SEARCH_MORPH" {
                    TokenizerKind::Morph
                } else {
                    TokenizerKind::NGram
                };

                if args.len() < 2 {
                    return Err(H2Error::Execution(format!("{} requires 2 arguments", func_name)));
                }

                let col_val = evaluate_func_arg_context(&args[0], ctx, row)?;
                let query_val = evaluate_func_arg_context(&args[1], ctx, row)?;

                match (&col_val, &query_val) {
                    (Value::String(text), Value::String(query)) => {
                        let tokenizer = get_tokenizer(kind);
                        let query_tokens = tokenizer.tokenize(query);
                        let doc_tokens = tokenizer.tokenize(text);

                        let matched = !query_tokens.is_empty()
                            && query_tokens.iter().all(|q_tok| doc_tokens.contains(q_tok));
                        Ok(Value::Boolean(matched))
                    }
                    _ => Ok(Value::Boolean(false)),
                }
            } else if func_name == "JSON_EXTRACT" || func_name == "JSON_VALUE" {
                if args.len() < 2 {
                    return Err(H2Error::Execution(format!("{} requires 2 arguments", func_name)));
                }
                let json_val = evaluate_func_arg_context(&args[0], ctx, row)?;
                let path_val = evaluate_func_arg_context(&args[1], ctx, row)?;

                let path_str = match &path_val {
                    Value::String(s) => s.as_str(),
                    _ => return Err(H2Error::Execution("JSON path must be a string".to_string())),
                };

                let serde_val = match &json_val {
                    Value::Json(j) => j.clone(),
                    Value::String(s) => match serde_json::from_str(s) {
                        Ok(j) => j,
                        Err(_) => return Ok(Value::Null),
                    },
                    _ => return Ok(Value::Null),
                };

                if let Some(target) = extract_json_path(&serde_val, path_str) {
                    Ok(json_to_value(target))
                } else {
                    Ok(Value::Null)
                }
            } else if func_name == "COALESCE" || func_name == "NVL" || func_name == "IFNULL" || func_name == "ISNULL" {
                for arg in args {
                    let val = evaluate_func_arg_context(arg, ctx, row)?;
                    if !matches!(val, Value::Null) {
                        return Ok(val);
                    }
                }
                Ok(Value::Null)
            } else if func_name == "NVL2" {
                if args.len() < 3 {
                    return Err(H2Error::Execution("NVL2 requires 3 arguments (expr, expr_if_not_null, expr_if_null)".to_string()));
                }
                let val = evaluate_func_arg_context(&args[0], ctx, row)?;
                if !matches!(val, Value::Null) {
                    evaluate_func_arg_context(&args[1], ctx, row)
                } else {
                    evaluate_func_arg_context(&args[2], ctx, row)
                }
            } else if func_name == "IF" {
                if args.len() < 3 {
                    return Err(H2Error::Execution("IF requires 3 arguments (condition, expr_true, expr_false)".to_string()));
                }
                let cond = evaluate_func_arg_context(&args[0], ctx, row)?;
                let is_true = match cond {
                    Value::Boolean(b) => b,
                    Value::Integer(i) => i != 0,
                    Value::BigInt(i) => i != 0,
                    Value::TinyInt(i) => i != 0,
                    Value::SmallInt(i) => i != 0,
                    Value::Null => false,
                    _ => false,
                };
                if is_true {
                    evaluate_func_arg_context(&args[1], ctx, row)
                } else {
                    evaluate_func_arg_context(&args[2], ctx, row)
                }
            } else if func_name == "DECODE" {
                if args.len() < 3 {
                    return Err(H2Error::Execution("DECODE requires at least 3 arguments".to_string()));
                }
                let base = evaluate_func_arg_context(&args[0], ctx, row)?;
                let mut i = 1;
                let mut matched_val = None;
                while i + 1 < args.len() {
                    let search = evaluate_func_arg_context(&args[i], ctx, row)?;
                    let is_match = (base == search) || (matches!(base, Value::Null) && matches!(search, Value::Null));
                    if is_match {
                        matched_val = Some(evaluate_func_arg_context(&args[i + 1], ctx, row)?);
                        break;
                    }
                    i += 2;
                }
                if let Some(res) = matched_val {
                    Ok(res)
                } else if i < args.len() {
                    evaluate_func_arg_context(&args[i], ctx, row)
                } else {
                    Ok(Value::Null)
                }
            } else if func_name == "UNIX_TIMESTAMP" {
                if args.is_empty() {
                    Ok(Value::BigInt(chrono::Utc::now().timestamp()))
                } else {
                    let v = evaluate_func_arg_context(&args[0], ctx, row)?;
                    match v {
                        Value::Timestamp(ts) => Ok(Value::BigInt(ts.timestamp())),
                        Value::Date(d) => {
                            let dt = d.and_hms_opt(0, 0, 0).unwrap().and_utc();
                            Ok(Value::BigInt(dt.timestamp()))
                        }
                        Value::String(s) => {
                            if let Ok(ts) = chrono::DateTime::parse_from_rfc3339(&s) {
                                Ok(Value::BigInt(ts.timestamp()))
                            } else if let Ok(naive) = chrono::NaiveDateTime::parse_from_str(&s, "%Y-%m-%d %H:%M:%S") {
                                Ok(Value::BigInt(naive.and_utc().timestamp()))
                            } else if let Ok(d) = chrono::NaiveDate::parse_from_str(&s, "%Y-%m-%d") {
                                Ok(Value::BigInt(d.and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp()))
                            } else {
                                Ok(Value::Null)
                            }
                        }
                        _ => Ok(Value::Null),
                    }
                }
            } else if func_name == "FROM_UNIXTIME" {
                if args.is_empty() {
                    return Err(H2Error::Execution("FROM_UNIXTIME requires 1 argument".to_string()));
                }
                let sec_val = evaluate_func_arg_context(&args[0], ctx, row)?;
                let sec = match sec_val {
                    Value::Integer(i) => i as i64,
                    Value::BigInt(i) => i,
                    Value::Double(d) => d as i64,
                    _ => return Ok(Value::Null),
                };
                if let Some(dt) = chrono::DateTime::from_timestamp(sec, 0) {
                    Ok(Value::Timestamp(dt.with_timezone(&chrono::Utc)))
                } else {
                    Ok(Value::Null)
                }
            } else if func_name == "CONCAT_WS" {
                if args.is_empty() {
                    return Err(H2Error::Execution("CONCAT_WS requires at least 1 argument".to_string()));
                }
                let sep_val = evaluate_func_arg_context(&args[0], ctx, row)?;
                let sep = match sep_val {
                    Value::String(s) => s,
                    Value::Null => return Ok(Value::Null),
                    v => v.to_string(),
                };
                let mut parts = Vec::new();
                for arg in &args[1..] {
                    let v = evaluate_func_arg_context(arg, ctx, row)?;
                    if !matches!(v, Value::Null) {
                        parts.push(match v {
                            Value::String(s) => s,
                            _ => v.to_string(),
                        });
                    }
                }
                Ok(Value::String(parts.join(&sep)))
            } else if func_name == "LEN" {
                if args.is_empty() { return Err(H2Error::Execution("LEN requires 1 argument".to_string())); }
                let v = evaluate_func_arg_context(&args[0], ctx, row)?;
                match v {
                    Value::String(s) => Ok(Value::Integer(s.chars().count() as i32)),
                    Value::Null => Ok(Value::Null),
                    _ => Ok(Value::Integer(v.to_string().chars().count() as i32)),
                }
            } else if func_name == "CHARINDEX" {
                if args.len() < 2 { return Err(H2Error::Execution("CHARINDEX requires at least 2 arguments".to_string())); }
                let sub_val = evaluate_func_arg_context(&args[0], ctx, row)?;
                let str_val = evaluate_func_arg_context(&args[1], ctx, row)?;
                let start_pos = if args.len() >= 3 {
                    evaluate_func_arg_context(&args[2], ctx, row)?.to_i64().unwrap_or(1)
                } else {
                    1
                };
                match (sub_val, str_val) {
                    (Value::String(sub), Value::String(s)) => {
                        if sub.is_empty() { return Ok(Value::Integer(1)); }
                        let start_idx = if start_pos > 1 { (start_pos - 1) as usize } else { 0 };
                        if start_idx >= s.len() {
                            Ok(Value::Integer(0))
                        } else if let Some(pos) = s[start_idx..].find(&sub) {
                            Ok(Value::Integer((start_idx + pos + 1) as i32))
                        } else {
                            Ok(Value::Integer(0))
                        }
                    }
                    _ => Ok(Value::Null),
                }
            } else if func_name == "INSTR" {
                if args.len() < 2 { return Err(H2Error::Execution("INSTR requires at least 2 arguments".to_string())); }
                let str_val = evaluate_func_arg_context(&args[0], ctx, row)?;
                let sub_val = evaluate_func_arg_context(&args[1], ctx, row)?;
                let start_pos = if args.len() >= 3 {
                    evaluate_func_arg_context(&args[2], ctx, row)?.to_i64().unwrap_or(1)
                } else {
                    1
                };
                match (str_val, sub_val) {
                    (Value::String(s), Value::String(sub)) => {
                        if sub.is_empty() { return Ok(Value::Integer(1)); }
                        let start_idx = if start_pos > 1 { (start_pos - 1) as usize } else { 0 };
                        if start_idx >= s.len() {
                            Ok(Value::Integer(0))
                        } else if let Some(pos) = s[start_idx..].find(&sub) {
                            Ok(Value::Integer((start_idx + pos + 1) as i32))
                        } else {
                            Ok(Value::Integer(0))
                        }
                    }
                    _ => Ok(Value::Null),
                }
            } else if func_name == "NEWID" {
                Ok(Value::String(uuid::Uuid::new_v4().to_string()))
            } else if func_name == "SQUARE" {
                if args.is_empty() { return Err(H2Error::Execution("SQUARE requires 1 argument".to_string())); }
                let v = evaluate_func_arg_context(&args[0], ctx, row)?;
                match v {
                    Value::Integer(i) => Ok(Value::Integer(i.wrapping_mul(i))),
                    Value::BigInt(i) => Ok(Value::BigInt(i.wrapping_mul(i))),
                    Value::Float(f) => Ok(Value::Float(f * f)),
                    Value::Double(d) => Ok(Value::Double(d * d)),
                    Value::Decimal(d) => Ok(Value::Decimal(d * d)),
                    Value::Null => Ok(Value::Null),
                    _ => Err(H2Error::TypeError("SQUARE requires numeric argument".to_string())),
                }
            } else if func_name == "DATABASE" || func_name == "SCHEMA" {
                Ok(Value::String("PUBLIC".to_string()))
            } else if func_name == "VERSION" {
                Ok(Value::String("H2Database-Rust 0.1.0".to_string()))
            } else if func_name == "CURRENT_MODE" {
                let mode_str = ctx.dialect_mode.map(|m| m.as_str()).unwrap_or("REGULAR");
                Ok(Value::String(mode_str.to_string()))
            } else if func_name == "TO_CHAR" {
                if args.is_empty() { return Err(H2Error::Execution("TO_CHAR requires at least 1 argument".to_string())); }
                let v = evaluate_func_arg_context(&args[0], ctx, row)?;
                match v {
                    Value::Null => Ok(Value::Null),
                    Value::String(s) => Ok(Value::String(s)),
                    Value::Date(d) => {
                        if args.len() >= 2 {
                            if let Value::String(fmt) = evaluate_func_arg_context(&args[1], ctx, row)? {
                                let rust_fmt = fmt.replace("YYYY", "%Y").replace("MM", "%m").replace("DD", "%d");
                                Ok(Value::String(d.format(&rust_fmt).to_string()))
                            } else {
                                Ok(Value::String(d.to_string()))
                            }
                        } else {
                            Ok(Value::String(d.to_string()))
                        }
                    }
                    Value::Timestamp(ts) => {
                        if args.len() >= 2 {
                            if let Value::String(fmt) = evaluate_func_arg_context(&args[1], ctx, row)? {
                                let rust_fmt = fmt.replace("YYYY", "%Y").replace("MM", "%m").replace("DD", "%d")
                                    .replace("HH24", "%H").replace("MI", "%M").replace("SS", "%S");
                                Ok(Value::String(ts.format(&rust_fmt).to_string()))
                            } else {
                                Ok(Value::String(ts.to_string()))
                            }
                        } else {
                            Ok(Value::String(ts.to_string()))
                        }
                    }
                    _ => Ok(Value::String(v.to_string())),
                }
            } else if func_name == "TO_DATE" {
                if args.is_empty() { return Err(H2Error::Execution("TO_DATE requires at least 1 argument".to_string())); }
                let s_val = evaluate_func_arg_context(&args[0], ctx, row)?;
                let s = match s_val {
                    Value::String(s) => s,
                    Value::Null => return Ok(Value::Null),
                    _ => s_val.to_string(),
                };
                if args.len() >= 2 {
                    if let Value::String(fmt) = evaluate_func_arg_context(&args[1], ctx, row)? {
                        let rust_fmt = fmt.replace("YYYY", "%Y").replace("MM", "%m").replace("DD", "%d");
                        if let Ok(d) = chrono::NaiveDate::parse_from_str(&s, &rust_fmt) {
                            return Ok(Value::Date(d));
                        }
                    }
                }
                if let Ok(d) = chrono::NaiveDate::parse_from_str(&s, "%Y-%m-%d") {
                    Ok(Value::Date(d))
                } else {
                    Err(H2Error::Execution(format!("Cannot parse date string '{}'", s)))
                }
            } else if func_name == "TO_NUMBER" {
                if args.is_empty() { return Err(H2Error::Execution("TO_NUMBER requires 1 argument".to_string())); }
                let v = evaluate_func_arg_context(&args[0], ctx, row)?;
                match v {
                    Value::Null => Ok(Value::Null),
                    Value::Integer(i) => Ok(Value::Integer(i)),
                    Value::BigInt(i) => Ok(Value::BigInt(i)),
                    Value::Decimal(d) => Ok(Value::Decimal(d)),
                    Value::Double(d) => Ok(Value::Double(d)),
                    Value::String(s) => {
                        let trimmed = s.trim();
                        if let Ok(i) = trimmed.parse::<i64>() {
                            if i >= i32::MIN as i64 && i <= i32::MAX as i64 {
                                Ok(Value::Integer(i as i32))
                            } else {
                                Ok(Value::BigInt(i))
                            }
                        } else if let Ok(d) = Decimal::from_str(trimmed) {
                            Ok(Value::Decimal(d))
                        } else if let Ok(f) = trimmed.parse::<f64>() {
                            Ok(Value::Double(f))
                        } else {
                            Err(H2Error::Execution(format!("Invalid number format for TO_NUMBER: '{}'", s)))
                        }
                    }
                    _ => Err(H2Error::TypeError("TO_NUMBER requires string or numeric argument".to_string())),
                }
            } else if func_name == "UPPER" {
                if args.is_empty() { return Err(H2Error::Execution("UPPER requires 1 argument".to_string())); }
                let v = evaluate_func_arg_context(&args[0], ctx, row)?;
                match v {
                    Value::String(s) => Ok(Value::String(s.to_uppercase())),
                    Value::Null => Ok(Value::Null),
                    _ => Ok(Value::String(v.to_string().to_uppercase())),
                }
            } else if func_name == "LOWER" {
                if args.is_empty() { return Err(H2Error::Execution("LOWER requires 1 argument".to_string())); }
                let v = evaluate_func_arg_context(&args[0], ctx, row)?;
                match v {
                    Value::String(s) => Ok(Value::String(s.to_lowercase())),
                    Value::Null => Ok(Value::Null),
                    _ => Ok(Value::String(v.to_string().to_lowercase())),
                }
            } else if func_name == "CONCAT" {
                let mut out = String::new();
                for arg in args {
                    let v = evaluate_func_arg_context(arg, ctx, row)?;
                    match v {
                        Value::String(s) => out.push_str(&s),
                        Value::Null => {}
                        _ => out.push_str(&v.to_string()),
                    }
                }
                Ok(Value::String(out))
            } else if func_name == "LENGTH" || func_name == "CHAR_LENGTH" {
                if args.is_empty() { return Err(H2Error::Execution("LENGTH requires 1 argument".to_string())); }
                let v = evaluate_func_arg_context(&args[0], ctx, row)?;
                match v {
                    Value::String(s) => Ok(Value::Integer(s.chars().count() as i32)),
                    Value::Null => Ok(Value::Null),
                    _ => Ok(Value::Integer(v.to_string().chars().count() as i32)),
                }
            } else if func_name == "ABS" {
                if args.is_empty() { return Err(H2Error::Execution("ABS requires 1 argument".to_string())); }
                let v = evaluate_func_arg_context(&args[0], ctx, row)?;
                match v {
                    Value::TinyInt(n) => Ok(Value::TinyInt(n.abs())),
                    Value::SmallInt(n) => Ok(Value::SmallInt(n.abs())),
                    Value::Integer(n) => Ok(Value::Integer(n.abs())),
                    Value::BigInt(n) => Ok(Value::BigInt(n.abs())),
                    Value::Float(f) => Ok(Value::Float(f.abs())),
                    Value::Double(d) => Ok(Value::Double(d.abs())),
                    Value::Decimal(d) => Ok(Value::Decimal(d.abs())),
                    Value::Null => Ok(Value::Null),
                    _ => Err(H2Error::TypeError("ABS requires numeric argument".to_string())),
                }
            } else if func_name == "NOW" || func_name == "CURRENT_TIMESTAMP" || func_name == "SYSDATE" || func_name == "GETDATE" {
                Ok(Value::Timestamp(chrono::Utc::now()))
            } else if func_name == "CURRENT_DATE" || func_name == "CURDATE" {
                Ok(Value::Date(chrono::Utc::now().date_naive()))
            } else if func_name == "CURRENT_TIME" || func_name == "CURTIME" {
                Ok(Value::Time(chrono::Utc::now().time()))
            } else if func_name == "RANDOM" {
                static SEED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(88172645463325252);
                let mut x = SEED.load(std::sync::atomic::Ordering::Relaxed);
                if x == 0 { x = 88172645463325252; }
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                SEED.store(x, std::sync::atomic::Ordering::Relaxed);
                let f = ((x >> 11) as f64) / ((1u64 << 53) as f64);
                Ok(Value::Double(f))
            // ================= 高度な数学関数 =================
            } else if func_name == "SIN" || func_name == "COS" || func_name == "TAN"
                || func_name == "ASIN" || func_name == "ACOS" || func_name == "ATAN" {
                if args.is_empty() { return Err(H2Error::Execution(format!("{} requires 1 argument", func_name))); }
                let v = evaluate_func_arg_context(&args[0], ctx, row)?;
                if v.is_null() { return Ok(Value::Null); }
                let f = v.to_f64().ok_or_else(|| H2Error::TypeError(format!("{} requires numeric argument", func_name)))?;
                let res = match func_name.as_str() {
                    "SIN" => f.sin(),
                    "COS" => f.cos(),
                    "TAN" => f.tan(),
                    "ASIN" => f.asin(),
                    "ACOS" => f.acos(),
                    "ATAN" => f.atan(),
                    _ => unreachable!(),
                };
                Ok(Value::Double(res))
            } else if func_name == "ATAN2" {
                if args.len() < 2 { return Err(H2Error::Execution("ATAN2 requires 2 arguments".to_string())); }
                let y = evaluate_func_arg_context(&args[0], ctx, row)?.to_f64().ok_or_else(|| H2Error::TypeError("ATAN2 requires numeric argument".to_string()))?;
                let x = evaluate_func_arg_context(&args[1], ctx, row)?.to_f64().ok_or_else(|| H2Error::TypeError("ATAN2 requires numeric argument".to_string()))?;
                Ok(Value::Double(y.atan2(x)))
            } else if func_name == "DEGREES" {
                if args.is_empty() { return Err(H2Error::Execution("DEGREES requires 1 argument".to_string())); }
                let f = evaluate_func_arg_context(&args[0], ctx, row)?.to_f64().ok_or_else(|| H2Error::TypeError("DEGREES requires numeric argument".to_string()))?;
                Ok(Value::Double(f.to_degrees()))
            } else if func_name == "RADIANS" {
                if args.is_empty() { return Err(H2Error::Execution("RADIANS requires 1 argument".to_string())); }
                let f = evaluate_func_arg_context(&args[0], ctx, row)?.to_f64().ok_or_else(|| H2Error::TypeError("RADIANS requires numeric argument".to_string()))?;
                Ok(Value::Double(f.to_radians()))
            } else if func_name == "PI" {
                Ok(Value::Double(std::f64::consts::PI))
            } else if func_name == "EXP" {
                if args.is_empty() { return Err(H2Error::Execution("EXP requires 1 argument".to_string())); }
                let f = evaluate_func_arg_context(&args[0], ctx, row)?.to_f64().ok_or_else(|| H2Error::TypeError("EXP requires numeric argument".to_string()))?;
                Ok(Value::Double(f.exp()))
            } else if func_name == "LN" {
                if args.is_empty() { return Err(H2Error::Execution("LN requires 1 argument".to_string())); }
                let f = evaluate_func_arg_context(&args[0], ctx, row)?.to_f64().ok_or_else(|| H2Error::TypeError("LN requires numeric argument".to_string()))?;
                if f <= 0.0 { return Err(H2Error::Execution("LN argument must be positive".to_string())); }
                Ok(Value::Double(f.ln()))
            } else if func_name == "LOG" || func_name == "LOG10" {
                if args.is_empty() { return Err(H2Error::Execution(format!("{} requires at least 1 argument", func_name))); }
                if func_name == "LOG10" || args.len() == 1 {
                    let f = evaluate_func_arg_context(&args[0], ctx, row)?.to_f64().ok_or_else(|| H2Error::TypeError("LOG requires numeric argument".to_string()))?;
                    if f <= 0.0 { return Err(H2Error::Execution("LOG argument must be positive".to_string())); }
                    if func_name == "LOG10" {
                        Ok(Value::Double(f.log10()))
                    } else {
                        Ok(Value::Double(f.ln()))
                    }
                } else {
                    let b = evaluate_func_arg_context(&args[0], ctx, row)?.to_f64().ok_or_else(|| H2Error::TypeError("LOG requires numeric argument".to_string()))?;
                    let x = evaluate_func_arg_context(&args[1], ctx, row)?.to_f64().ok_or_else(|| H2Error::TypeError("LOG requires numeric argument".to_string()))?;
                    if b <= 0.0 || b == 1.0 || x <= 0.0 { return Err(H2Error::Execution("Invalid base or argument for LOG".to_string())); }
                    Ok(Value::Double(x.log(b)))
                }
            } else if func_name == "SQRT" {
                if args.is_empty() { return Err(H2Error::Execution("SQRT requires 1 argument".to_string())); }
                let f = evaluate_func_arg_context(&args[0], ctx, row)?.to_f64().ok_or_else(|| H2Error::TypeError("SQRT requires numeric argument".to_string()))?;
                if f < 0.0 { return Err(H2Error::Execution("Cannot take SQRT of negative number".to_string())); }
                Ok(Value::Double(f.sqrt()))
            } else if func_name == "POWER" || func_name == "POW" {
                if args.len() < 2 { return Err(H2Error::Execution(format!("{} requires 2 arguments", func_name))); }
                let x = evaluate_func_arg_context(&args[0], ctx, row)?.to_f64().ok_or_else(|| H2Error::TypeError("POWER requires numeric argument".to_string()))?;
                let y = evaluate_func_arg_context(&args[1], ctx, row)?.to_f64().ok_or_else(|| H2Error::TypeError("POWER requires numeric argument".to_string()))?;
                Ok(Value::Double(x.powf(y)))
            } else if func_name == "CEIL" || func_name == "CEILING" {
                if args.is_empty() { return Err(H2Error::Execution("CEIL requires 1 argument".to_string())); }
                let v = evaluate_func_arg_context(&args[0], ctx, row)?;
                match v {
                    Value::Integer(i) => Ok(Value::Integer(i)),
                    Value::BigInt(i) => Ok(Value::BigInt(i)),
                    Value::Float(f) => Ok(Value::Float(f.ceil())),
                    Value::Double(d) => Ok(Value::Double(d.ceil())),
                    Value::Decimal(d) => Ok(Value::Decimal(d.ceil())),
                    _ => Err(H2Error::TypeError("CEIL requires numeric argument".to_string())),
                }
            } else if func_name == "FLOOR" {
                if args.is_empty() { return Err(H2Error::Execution("FLOOR requires 1 argument".to_string())); }
                let v = evaluate_func_arg_context(&args[0], ctx, row)?;
                match v {
                    Value::Integer(i) => Ok(Value::Integer(i)),
                    Value::BigInt(i) => Ok(Value::BigInt(i)),
                    Value::Float(f) => Ok(Value::Float(f.floor())),
                    Value::Double(d) => Ok(Value::Double(d.floor())),
                    Value::Decimal(d) => Ok(Value::Decimal(d.floor())),
                    _ => Err(H2Error::TypeError("FLOOR requires numeric argument".to_string())),
                }
            } else if func_name == "ROUND" {
                if args.is_empty() { return Err(H2Error::Execution("ROUND requires at least 1 argument".to_string())); }
                let v = evaluate_func_arg_context(&args[0], ctx, row)?;
                let digits = if args.len() >= 2 {
                    evaluate_func_arg_context(&args[1], ctx, row)?.to_i64().unwrap_or(0) as u32
                } else {
                    0
                };
                if v.is_null() { return Ok(Value::Null); }
                if let Some(mut d) = v.to_decimal() {
                    d = d.round_dp(digits);
                    if digits == 0 {
                        if let Some(i) = d.to_i64() {
                            if i >= i32::MIN as i64 && i <= i32::MAX as i64 {
                                return Ok(Value::Integer(i as i32));
                            }
                            return Ok(Value::BigInt(i));
                        }
                    }
                    Ok(Value::Decimal(d))
                } else if let Some(f) = v.to_f64() {
                    let factor = 10f64.powi(digits as i32);
                    Ok(Value::Double((f * factor).round() / factor))
                } else {
                    Err(H2Error::TypeError("ROUND requires numeric argument".to_string()))
                }
            } else if func_name == "TRUNC" || func_name == "TRUNCATE" {
                if args.is_empty() { return Err(H2Error::Execution("TRUNC requires at least 1 argument".to_string())); }
                let v = evaluate_func_arg_context(&args[0], ctx, row)?;
                let digits = if args.len() >= 2 {
                    evaluate_func_arg_context(&args[1], ctx, row)?.to_i64().unwrap_or(0) as u32
                } else {
                    0
                };
                if v.is_null() { return Ok(Value::Null); }
                if let Some(mut d) = v.to_decimal() {
                    d = d.trunc_with_scale(digits);
                    if digits == 0 {
                        if let Some(i) = d.to_i64() {
                            if i >= i32::MIN as i64 && i <= i32::MAX as i64 {
                                return Ok(Value::Integer(i as i32));
                            }
                            return Ok(Value::BigInt(i));
                        }
                    }
                    Ok(Value::Decimal(d))
                } else if let Some(f) = v.to_f64() {
                    let factor = 10f64.powi(digits as i32);
                    Ok(Value::Double((f * factor).trunc() / factor))
                } else {
                    Err(H2Error::TypeError("TRUNC requires numeric argument".to_string()))
                }
            } else if func_name == "SIGN" {
                if args.is_empty() { return Err(H2Error::Execution("SIGN requires 1 argument".to_string())); }
                let f = evaluate_func_arg_context(&args[0], ctx, row)?.to_f64().ok_or_else(|| H2Error::TypeError("SIGN requires numeric argument".to_string()))?;
                let s = if f > 0.0 { 1 } else if f < 0.0 { -1 } else { 0 };
                Ok(Value::Integer(s))
            } else if func_name == "MOD" {
                if args.len() < 2 { return Err(H2Error::Execution("MOD requires 2 arguments".to_string())); }
                let a = evaluate_func_arg_context(&args[0], ctx, row)?;
                let b = evaluate_func_arg_context(&args[1], ctx, row)?;
                evaluate_arithmetic_op(&a, &BinaryOperator::Modulo, &b)
            // ================= 高度な文字列関数 =================
            } else if func_name == "SUBSTR" || func_name == "SUBSTRING" {
                if args.len() < 2 { return Err(H2Error::Execution("SUBSTRING requires at least 2 arguments".to_string())); }
                let s = evaluate_func_arg_context(&args[0], ctx, row)?;
                let from = evaluate_func_arg_context(&args[1], ctx, row)?;
                let len = if args.len() >= 3 { Some(evaluate_func_arg_context(&args[2], ctx, row)?) } else { None };
                match s {
                    Value::String(str_val) => substring_impl(&str_val, &from, len.as_ref()),
                    Value::Null => Ok(Value::Null),
                    _ => substring_impl(&s.to_string(), &from, len.as_ref()),
                }
            } else if func_name == "TRIM" || func_name == "LTRIM" || func_name == "RTRIM" || func_name == "BTRIM" {
                if args.is_empty() { return Err(H2Error::Execution(format!("{} requires at least 1 argument", func_name))); }
                let s_val = evaluate_func_arg_context(&args[0], ctx, row)?;
                let s = match s_val {
                    Value::String(s) => s,
                    Value::Null => return Ok(Value::Null),
                    _ => s_val.to_string(),
                };
                let chars = if args.len() >= 2 {
                    match evaluate_func_arg_context(&args[1], ctx, row)? {
                        Value::String(c) => c,
                        _ => " ".to_string(),
                    }
                } else {
                    " ".to_string()
                };
                let trimmed = match func_name.as_str() {
                    "LTRIM" => s.trim_start_matches(|c| chars.contains(c)).to_string(),
                    "RTRIM" => s.trim_end_matches(|c| chars.contains(c)).to_string(),
                    _ => s.trim_matches(|c| chars.contains(c)).to_string(),
                };
                Ok(Value::String(trimmed))
            } else if func_name == "REPLACE" {
                if args.len() < 3 { return Err(H2Error::Execution("REPLACE requires 3 arguments".to_string())); }
                let s = evaluate_func_arg_context(&args[0], ctx, row)?;
                let from = evaluate_func_arg_context(&args[1], ctx, row)?;
                let to = evaluate_func_arg_context(&args[2], ctx, row)?;
                match (s, from, to) {
                    (Value::String(s), Value::String(f), Value::String(t)) => Ok(Value::String(s.replace(&f, &t))),
                    _ => Ok(Value::Null),
                }
            } else if func_name == "LPAD" || func_name == "RPAD" {
                if args.len() < 2 { return Err(H2Error::Execution(format!("{} requires at least 2 arguments", func_name))); }
                let s = evaluate_func_arg_context(&args[0], ctx, row)?;
                let len = evaluate_func_arg_context(&args[1], ctx, row)?.to_i64().unwrap_or(0) as usize;
                let pad = if args.len() >= 3 {
                    match evaluate_func_arg_context(&args[2], ctx, row)? {
                        Value::String(p) => p,
                        _ => " ".to_string(),
                    }
                } else {
                    " ".to_string()
                };
                let s_str = match s { Value::String(s) => s, Value::Null => return Ok(Value::Null), v => v.to_string() };
                let char_count = s_str.chars().count();
                if char_count >= len {
                    let truncated: String = s_str.chars().take(len).collect();
                    Ok(Value::String(truncated))
                } else {
                    let diff = len - char_count;
                    let pad_repeated: String = pad.chars().cycle().take(diff).collect();
                    if func_name == "LPAD" {
                        Ok(Value::String(format!("{}{}", pad_repeated, s_str)))
                    } else {
                        Ok(Value::String(format!("{}{}", s_str, pad_repeated)))
                    }
                }
            } else if func_name == "INITCAP" {
                if args.is_empty() { return Err(H2Error::Execution("INITCAP requires 1 argument".to_string())); }
                let s = evaluate_func_arg_context(&args[0], ctx, row)?;
                match s {
                    Value::String(s) => Ok(Value::String(initcap(&s))),
                    Value::Null => Ok(Value::Null),
                    _ => Ok(Value::String(initcap(&s.to_string()))),
                }
            } else if func_name == "REVERSE" {
                if args.is_empty() { return Err(H2Error::Execution("REVERSE requires 1 argument".to_string())); }
                let s = evaluate_func_arg_context(&args[0], ctx, row)?;
                match s {
                    Value::String(s) => Ok(Value::String(s.chars().rev().collect())),
                    Value::Null => Ok(Value::Null),
                    _ => Ok(Value::String(s.to_string().chars().rev().collect())),
                }
            } else if func_name == "REPEAT" {
                if args.len() < 2 { return Err(H2Error::Execution("REPEAT requires 2 arguments".to_string())); }
                let s = evaluate_func_arg_context(&args[0], ctx, row)?;
                let n = evaluate_func_arg_context(&args[1], ctx, row)?.to_i64().unwrap_or(0);
                if n <= 0 { return Ok(Value::String(String::new())); }
                match s {
                    Value::String(s) => Ok(Value::String(s.repeat(n as usize))),
                    Value::Null => Ok(Value::Null),
                    _ => Ok(Value::String(s.to_string().repeat(n as usize))),
                }
            } else if func_name == "TRANSLATE" {
                if args.len() < 3 { return Err(H2Error::Execution("TRANSLATE requires 3 arguments".to_string())); }
                let s = evaluate_func_arg_context(&args[0], ctx, row)?;
                let from = evaluate_func_arg_context(&args[1], ctx, row)?;
                let to = evaluate_func_arg_context(&args[2], ctx, row)?;
                match (s, from, to) {
                    (Value::String(s), Value::String(f), Value::String(t)) => Ok(Value::String(translate(&s, &f, &t))),
                    _ => Ok(Value::Null),
                }
            } else if func_name == "SPLIT_PART" {
                if args.len() < 3 { return Err(H2Error::Execution("SPLIT_PART requires 3 arguments".to_string())); }
                let s = evaluate_func_arg_context(&args[0], ctx, row)?;
                let delim = evaluate_func_arg_context(&args[1], ctx, row)?;
                let part = evaluate_func_arg_context(&args[2], ctx, row)?.to_i64().unwrap_or(1);
                match (s, delim) {
                    (Value::String(s), Value::String(d)) => {
                        if part <= 0 { return Ok(Value::String(String::new())); }
                        let idx = (part - 1) as usize;
                        let parts: Vec<&str> = s.split(&d).collect();
                        if idx < parts.len() {
                            Ok(Value::String(parts[idx].to_string()))
                        } else {
                            Ok(Value::String(String::new()))
                        }
                    }
                    _ => Ok(Value::Null),
                }
            } else if func_name == "LEFT" || func_name == "RIGHT" {
                if args.len() < 2 { return Err(H2Error::Execution(format!("{} requires 2 arguments", func_name))); }
                let s = evaluate_func_arg_context(&args[0], ctx, row)?;
                let n = evaluate_func_arg_context(&args[1], ctx, row)?.to_i64().unwrap_or(0);
                let s_str = match s { Value::String(s) => s, Value::Null => return Ok(Value::Null), v => v.to_string() };
                let chars: Vec<char> = s_str.chars().collect();
                if n <= 0 { return Ok(Value::String(String::new())); }
                let n_usize = n as usize;
                if func_name == "LEFT" {
                    let res: String = chars.into_iter().take(n_usize).collect();
                    Ok(Value::String(res))
                } else {
                    let skip = if chars.len() > n_usize { chars.len() - n_usize } else { 0 };
                    let res: String = chars.into_iter().skip(skip).collect();
                    Ok(Value::String(res))
                }
            } else if func_name == "CHR" {
                if args.is_empty() { return Err(H2Error::Execution("CHR requires 1 argument".to_string())); }
                let code = evaluate_func_arg_context(&args[0], ctx, row)?.to_i64().unwrap_or(0) as u32;
                let c = char::from_u32(code).unwrap_or('\0');
                Ok(Value::String(c.to_string()))
            } else if func_name == "ASCII" {
                if args.is_empty() { return Err(H2Error::Execution("ASCII requires 1 argument".to_string())); }
                let s = evaluate_func_arg_context(&args[0], ctx, row)?;
                match s {
                    Value::String(s) => {
                        if let Some(c) = s.chars().next() {
                            Ok(Value::Integer(c as i32))
                        } else {
                            Ok(Value::Integer(0))
                        }
                    }
                    _ => Ok(Value::Null),
                }
            // ================= 正規表現関数 =================
            } else if func_name == "REGEXP_LIKE" {
                if args.len() < 2 { return Err(H2Error::Execution("REGEXP_LIKE requires at least 2 arguments".to_string())); }
                let text = evaluate_func_arg_context(&args[0], ctx, row)?;
                let pat = evaluate_func_arg_context(&args[1], ctx, row)?;
                let case_insensitive = if args.len() >= 3 {
                    match evaluate_func_arg_context(&args[2], ctx, row)? {
                        Value::String(flags) => flags.contains('i'),
                        _ => false,
                    }
                } else {
                    false
                };
                match (text, pat) {
                    (Value::String(t), Value::String(p)) => regex_match(&p, &t, case_insensitive).map(Value::Boolean),
                    _ => Ok(Value::Null),
                }
            } else if func_name == "REGEXP_REPLACE" {
                if args.len() < 3 { return Err(H2Error::Execution("REGEXP_REPLACE requires at least 3 arguments".to_string())); }
                let text = evaluate_func_arg_context(&args[0], ctx, row)?;
                let pat = evaluate_func_arg_context(&args[1], ctx, row)?;
                let rep = evaluate_func_arg_context(&args[2], ctx, row)?;
                let flags = if args.len() >= 4 {
                    match evaluate_func_arg_context(&args[3], ctx, row)? {
                        Value::String(f) => f,
                        _ => String::new(),
                    }
                } else {
                    String::new()
                };
                match (text, pat, rep) {
                    (Value::String(t), Value::String(p), Value::String(r)) => {
                        let mut builder = regex::RegexBuilder::new(&p);
                        builder.case_insensitive(flags.contains('i'));
                        let re = builder.build().map_err(|e| H2Error::Execution(format!("Invalid regex '{}': {}", p, e)))?;
                        if flags.contains('g') {
                            Ok(Value::String(re.replace_all(&t, r.as_str()).to_string()))
                        } else {
                            Ok(Value::String(re.replace(&t, r.as_str()).to_string()))
                        }
                    }
                    _ => Ok(Value::Null),
                }
            } else if func_name == "REGEXP_SUBSTR" {
                if args.len() < 2 { return Err(H2Error::Execution("REGEXP_SUBSTR requires 2 arguments".to_string())); }
                let text = evaluate_func_arg_context(&args[0], ctx, row)?;
                let pat = evaluate_func_arg_context(&args[1], ctx, row)?;
                match (text, pat) {
                    (Value::String(t), Value::String(p)) => {
                        let re = regex::Regex::new(&p).map_err(|e| H2Error::Execution(format!("Invalid regex '{}': {}", p, e)))?;
                        if let Some(m) = re.find(&t) {
                            Ok(Value::String(m.as_str().to_string()))
                        } else {
                            Ok(Value::Null)
                        }
                    }
                    _ => Ok(Value::Null),
                }
            // ================= 日付計算関数 =================
            } else if func_name == "DATE_ADD" || func_name == "DATE_SUB" {
                if args.len() < 2 { return Err(H2Error::Execution(format!("{} requires 2 arguments", func_name))); }
                let d = evaluate_func_arg_context(&args[0], ctx, row)?;
                let iv_val = evaluate_func_arg_context(&args[1], ctx, row)?;
                let iv = match iv_val {
                    Value::Interval(iv) => iv,
                    Value::String(s) => IntervalValue::parse(&s).ok_or_else(|| H2Error::TypeError(format!("Invalid INTERVAL string '{}'", s)))?,
                    _ => return Err(H2Error::TypeError("Second argument must be INTERVAL".to_string())),
                };
                let op = if func_name == "DATE_ADD" { BinaryOperator::Plus } else { BinaryOperator::Minus };
                evaluate_arithmetic_op(&d, &op, &Value::Interval(iv))
            } else if func_name == "DATEDIFF" {
                if args.len() < 2 { return Err(H2Error::Execution("DATEDIFF requires at least 2 arguments".to_string())); }
                let (unit, d1_arg, d2_arg) = if args.len() >= 3 {
                    let u = match evaluate_func_arg_context(&args[0], ctx, row)? {
                        Value::String(s) => s.to_uppercase(),
                        _ => "DAY".to_string(),
                    };
                    (u, &args[1], &args[2])
                } else {
                    ("DAY".to_string(), &args[0], &args[1])
                };
                let d1 = evaluate_func_arg_context(d1_arg, ctx, row)?;
                let d2 = evaluate_func_arg_context(d2_arg, ctx, row)?;
                let diff = evaluate_arithmetic_op(&d1, &BinaryOperator::Minus, &d2)?;
                match diff {
                    Value::Interval(iv) => match unit.as_str() {
                        "YEAR" | "YEARS" => Ok(Value::Integer(iv.months / 12)),
                        "MONTH" | "MONTHS" => Ok(Value::Integer(iv.months)),
                        "DAY" | "DAYS" => Ok(Value::Integer(iv.days)),
                        "HOUR" | "HOURS" => Ok(Value::Integer((iv.microseconds / 3_600_000_000) as i32)),
                        "MINUTE" | "MINUTES" => Ok(Value::Integer((iv.microseconds / 60_000_000) as i32)),
                        "SECOND" | "SECONDS" => Ok(Value::Integer((iv.microseconds / 1_000_000) as i32)),
                        _ => Ok(Value::Integer(iv.days)),
                    },
                    _ => Err(H2Error::Execution("Cannot compute DATEDIFF".to_string())),
                }
            } else if func_name == "DATE_PART" {
                if args.len() < 2 { return Err(H2Error::Execution("DATE_PART requires 2 arguments".to_string())); }
                let unit = match evaluate_func_arg_context(&args[0], ctx, row)? {
                    Value::String(s) => s,
                    _ => return Err(H2Error::TypeError("First argument to DATE_PART must be string".to_string())),
                };
                let target = evaluate_func_arg_context(&args[1], ctx, row)?;
                extract_field(&target, &unit)
            } else if func_name == "DATE_TRUNC" {
                if args.len() < 2 { return Err(H2Error::Execution("DATE_TRUNC requires 2 arguments".to_string())); }
                let unit = match evaluate_func_arg_context(&args[0], ctx, row)? {
                    Value::String(s) => s.to_uppercase(),
                    _ => return Err(H2Error::TypeError("First argument to DATE_TRUNC must be string".to_string())),
                };
                let target = evaluate_func_arg_context(&args[1], ctx, row)?;
                match target {
                    Value::Date(d) => match unit.as_str() {
                        "YEAR" => Ok(Value::Date(chrono::NaiveDate::from_ymd_opt(d.year(), 1, 1).unwrap())),
                        "MONTH" => Ok(Value::Date(chrono::NaiveDate::from_ymd_opt(d.year(), d.month(), 1).unwrap())),
                        "DAY" => Ok(Value::Date(d)),
                        _ => Ok(Value::Date(d)),
                    },
                    Value::Timestamp(ts) => {
                        use chrono::Timelike;
                        let d = ts.date_naive();
                        match unit.as_str() {
                            "YEAR" => {
                                let nd = chrono::NaiveDate::from_ymd_opt(d.year(), 1, 1).unwrap();
                                let nt = chrono::NaiveTime::from_hms_opt(0, 0, 0).unwrap();
                                Ok(Value::Timestamp(chrono::DateTime::from_naive_utc_and_offset(chrono::NaiveDateTime::new(nd, nt), chrono::Utc)))
                            }
                            "MONTH" => {
                                let nd = chrono::NaiveDate::from_ymd_opt(d.year(), d.month(), 1).unwrap();
                                let nt = chrono::NaiveTime::from_hms_opt(0, 0, 0).unwrap();
                                Ok(Value::Timestamp(chrono::DateTime::from_naive_utc_and_offset(chrono::NaiveDateTime::new(nd, nt), chrono::Utc)))
                            }
                            "DAY" => {
                                let nt = chrono::NaiveTime::from_hms_opt(0, 0, 0).unwrap();
                                Ok(Value::Timestamp(chrono::DateTime::from_naive_utc_and_offset(chrono::NaiveDateTime::new(d, nt), chrono::Utc)))
                            }
                            "HOUR" => {
                                let nt = chrono::NaiveTime::from_hms_opt(ts.hour(), 0, 0).unwrap();
                                Ok(Value::Timestamp(chrono::DateTime::from_naive_utc_and_offset(chrono::NaiveDateTime::new(d, nt), chrono::Utc)))
                            }
                            "MINUTE" => {
                                let nt = chrono::NaiveTime::from_hms_opt(ts.hour(), ts.minute(), 0).unwrap();
                                Ok(Value::Timestamp(chrono::DateTime::from_naive_utc_and_offset(chrono::NaiveDateTime::new(d, nt), chrono::Utc)))
                            }
                            _ => Ok(Value::Timestamp(ts)),
                        }
                    }
                    _ => Ok(target),
                }
            } else if func_name == "AGE" {
                if args.is_empty() { return Err(H2Error::Execution("AGE requires at least 1 argument".to_string())); }
                let t1 = evaluate_func_arg_context(&args[0], ctx, row)?;
                let t2 = if args.len() >= 2 {
                    evaluate_func_arg_context(&args[1], ctx, row)?
                } else {
                    Value::Timestamp(chrono::Utc::now())
                };
                evaluate_arithmetic_op(&t1, &BinaryOperator::Minus, &t2)
            } else if func_name == "NEXTVAL" {
                if args.is_empty() { return Err(H2Error::Execution("NEXTVAL requires 1 argument (sequence name)".to_string())); }
                let seq_val = evaluate_func_arg_context(&args[0], ctx, row)?;
                let seq_name = match &seq_val {
                    Value::String(s) => s.as_str(),
                    _ => return Err(H2Error::Execution("NEXTVAL argument must be a string".to_string())),
                };
                let catalog = ctx.catalog.as_ref().ok_or_else(|| {
                    H2Error::Execution("Catalog context unavailable for NEXTVAL".to_string())
                })?;
                let v = catalog.nextval(seq_name)?;
                Ok(Value::BigInt(v))
            } else if func_name == "CURRVAL" {
                if args.is_empty() { return Err(H2Error::Execution("CURRVAL requires 1 argument (sequence name)".to_string())); }
                let seq_val = evaluate_func_arg_context(&args[0], ctx, row)?;
                let seq_name = match &seq_val {
                    Value::String(s) => s.as_str(),
                    _ => return Err(H2Error::Execution("CURRVAL argument must be a string".to_string())),
                };
                let catalog = ctx.catalog.as_ref().ok_or_else(|| {
                    H2Error::Execution("Catalog context unavailable for CURRVAL".to_string())
                })?;
                let v = catalog.currval(seq_name)?;
                Ok(Value::BigInt(v))
            } else if func_name == "SETVAL" {
                if args.len() < 2 { return Err(H2Error::Execution("SETVAL requires 2 arguments (sequence name, value)".to_string())); }
                let seq_val = evaluate_func_arg_context(&args[0], ctx, row)?;
                let seq_name = match &seq_val {
                    Value::String(s) => s.as_str(),
                    _ => return Err(H2Error::Execution("SETVAL argument 1 must be a string".to_string())),
                };
                let target_v = evaluate_func_arg_context(&args[1], ctx, row)?;
                let val_num = match target_v {
                    Value::Integer(i) => i as i64,
                    Value::BigInt(i) => i,
                    _ => return Err(H2Error::Execution("SETVAL argument 2 must be an integer".to_string())),
                };
                let is_called = if args.len() >= 3 {
                    match evaluate_func_arg_context(&args[2], ctx, row)? {
                        Value::Boolean(b) => b,
                        _ => true,
                    }
                } else {
                    true
                };
                let catalog = ctx.catalog.as_ref().ok_or_else(|| {
                    H2Error::Execution("Catalog context unavailable for SETVAL".to_string())
                })?;
                let v = catalog.setval(seq_name, val_num, is_called)?;
                Ok(Value::BigInt(v))
            } else if let Some(catalog) = &ctx.catalog {
                if let Some(routine) = catalog.get_routine(&func_name) {
                    if routine.kind == RoutineKind::Function {
                        let mut interp = ProcInterpreter::new(
                            Some(Arc::clone(catalog)),
                            None,
                            None,
                            None,
                        );
                        let mut arg_vals = Vec::new();
                        for a in args {
                            arg_vals.push(evaluate_func_arg_context(a, ctx, row)?);
                        }
                        let res = interp.execute_routine(&routine, &arg_vals)?;
                        if let ExecutionResult::Query { rows, .. } = res {
                            if let Some(first_row) = rows.first() {
                                if let Some(v) = first_row.values.first() {
                                    return Ok(v.clone());
                                }
                            }
                        }
                        Ok(Value::Null)
                    } else {
                        Err(H2Error::Execution(format!("'{}' is a procedure, not a function", func_name)))
                    }
                } else {
                    Err(H2Error::Execution(format!("Unsupported scalar function: {}", func_name)))
                }
            } else {
                Err(H2Error::Execution(format!("Unsupported scalar function: {}", func_name)))
            }
        }
        _ => Err(H2Error::Execution(format!("Unsupported expression: {:?}", expr))),
    }
}

fn evaluate_func_arg_context(arg: &FunctionArg, ctx: &RowContext, row: &Row) -> H2Result<Value> {
    match arg {
        FunctionArg::Unnamed(FunctionArgExpr::Expr(expr)) => evaluate_expr_context(expr, ctx, row),
        _ => Err(H2Error::Execution("Invalid function argument".to_string())),
    }
}


pub fn evaluate_sql_value(val: &SqlValue) -> H2Result<Value> {
    match val {
        SqlValue::Null => Ok(Value::Null),
        SqlValue::Boolean(b) => Ok(Value::Boolean(*b)),
        SqlValue::Number(num_str, _) => {
            if let Ok(i) = num_str.parse::<i64>() {
                if i >= i32::MIN as i64 && i <= i32::MAX as i64 {
                    Ok(Value::Integer(i as i32))
                } else {
                    Ok(Value::BigInt(i))
                }
            } else if let Ok(d) = Decimal::from_str(num_str) {
                Ok(Value::Decimal(d))
            } else if let Ok(f) = num_str.parse::<f64>() {
                Ok(Value::Double(f))
            } else {
                Err(H2Error::TypeError(format!("Invalid number literal: {}", num_str)))
            }
        }
        SqlValue::SingleQuotedString(s) | SqlValue::DoubleQuotedString(s) => {
            Ok(Value::String(s.clone()))
        }
        _ => Err(H2Error::TypeError(format!("Unsupported literal value: {:?}", val))),
    }
}

pub fn evaluate_literal_or_unary(expr: &SqlExpr) -> H2Result<Value> {
    match expr {
        SqlExpr::Value(v) => evaluate_sql_value(v),
        SqlExpr::UnaryOp { op, expr } => {
            let inner = evaluate_literal_or_unary(expr)?;
            match op {
                sqlparser::ast::UnaryOperator::Minus => match inner {
                    Value::TinyInt(n) => Ok(Value::TinyInt(-n)),
                    Value::SmallInt(n) => Ok(Value::SmallInt(-n)),
                    Value::Integer(n) => Ok(Value::Integer(-n)),
                    Value::BigInt(n) => Ok(Value::BigInt(-n)),
                    Value::Float(f) => Ok(Value::Float(-f)),
                    Value::Double(d) => Ok(Value::Double(-d)),
                    Value::Decimal(d) => Ok(Value::Decimal(-d)),
                    _ => Err(H2Error::TypeError("Cannot negate non-numeric value".to_string())),
                },
                sqlparser::ast::UnaryOperator::Plus => Ok(inner),
                sqlparser::ast::UnaryOperator::Not => match inner {
                    Value::Boolean(b) => Ok(Value::Boolean(!b)),
                    _ => Err(H2Error::TypeError("NOT requires boolean operand".to_string())),
                },
                _ => Err(H2Error::Execution(format!("Unsupported unary operator: {:?}", op))),
            }
        }
        SqlExpr::Tuple(exprs) => {
            let mut vals = Vec::with_capacity(exprs.len());
            for e in exprs {
                vals.push(evaluate_literal_or_unary(e)?);
            }
            Ok(Value::Array(vals))
        }
        SqlExpr::Interval(interval) => {
            let base_val = evaluate_literal_or_unary(&interval.value)?;
            let s = match &base_val {
                Value::String(s) => s.clone(),
                Value::Integer(i) => {
                    if let Some(ref field) = interval.leading_field {
                        format!("{} {:?}", i, field)
                    } else {
                        format!("{} second", i)
                    }
                }
                _ => base_val.to_string(),
            };
            let interval_str = if let Some(ref field) = interval.leading_field {
                if s.split_whitespace().count() == 1 {
                    format!("{} {:?}", s, field)
                } else {
                    s
                }
            } else {
                s
            };
            if let Some(iv) = IntervalValue::parse(&interval_str) {
                Ok(Value::Interval(iv))
            } else {
                Err(H2Error::Execution(format!("Invalid INTERVAL literal: '{}'", interval_str)))
            }
        }
        SqlExpr::TypedString { data_type, value } => {
            let target_type = crate::parser::convert_data_type(data_type)?;
            Value::String(value.clone()).cast_to(&target_type)
        }
        _ => Err(H2Error::Execution(format!("Unsupported literal or unary expression: {:?}", expr))),
    }
}

pub fn evaluate_binary_op(left: &Value, op: &BinaryOperator, right: &Value) -> H2Result<Value> {
    match op {
        BinaryOperator::Eq => Ok(Value::Boolean(left == right)),
        BinaryOperator::NotEq => Ok(Value::Boolean(left != right)),
        BinaryOperator::Lt => Ok(Value::Boolean(left < right)),
        BinaryOperator::LtEq => Ok(Value::Boolean(left <= right)),
        BinaryOperator::Gt => Ok(Value::Boolean(left > right)),
        BinaryOperator::GtEq => Ok(Value::Boolean(left >= right)),
        // MATCH 演算子 (例: title MATCH 'database')
        BinaryOperator::Custom(s) if s.eq_ignore_ascii_case("MATCH") => {
            match (left, right) {
                (Value::String(text), Value::String(query)) => {
                    let tokenizer = get_tokenizer(TokenizerKind::NGram);
                    let query_tokens = tokenizer.tokenize(query);
                    let doc_tokens = tokenizer.tokenize(text);
                    let matched = !query_tokens.is_empty()
                        && query_tokens.iter().all(|q_tok| doc_tokens.contains(q_tok));
                    Ok(Value::Boolean(matched))
                }
                _ => Ok(Value::Boolean(false)),
            }
        }
        BinaryOperator::And => {
            let l_bool = match left {
                Value::Boolean(b) => *b,
                _ => return Err(H2Error::TypeError("AND requires boolean operands".to_string())),
            };
            let r_bool = match right {
                Value::Boolean(b) => *b,
                _ => return Err(H2Error::TypeError("AND requires boolean operands".to_string())),
            };
            Ok(Value::Boolean(l_bool && r_bool))
        }
        BinaryOperator::Or => {
            let l_bool = match left {
                Value::Boolean(b) => *b,
                _ => return Err(H2Error::TypeError("OR requires boolean operands".to_string())),
            };
            let r_bool = match right {
                Value::Boolean(b) => *b,
                _ => return Err(H2Error::TypeError("OR requires boolean operands".to_string())),
            };
            Ok(Value::Boolean(l_bool || r_bool))
        }
        BinaryOperator::Plus | BinaryOperator::Minus | BinaryOperator::Multiply | BinaryOperator::Divide | BinaryOperator::Modulo => {
            evaluate_arithmetic_op(left, op, right)
        }
        BinaryOperator::StringConcat => {
            let l_str = match left { Value::String(s) => s.clone(), Value::Null => return Ok(Value::Null), v => v.to_string() };
            let r_str = match right { Value::String(s) => s.clone(), Value::Null => return Ok(Value::Null), v => v.to_string() };
            Ok(Value::String(format!("{}{}", l_str, r_str)))
        }
        BinaryOperator::PGRegexMatch => {
            let (l, r) = match (left, right) {
                (Value::String(l), Value::String(r)) => (l.as_str(), r.as_str()),
                _ => return Ok(Value::Null),
            };
            regex_match(r, l, false).map(Value::Boolean)
        }
        BinaryOperator::PGRegexIMatch => {
            let (l, r) = match (left, right) {
                (Value::String(l), Value::String(r)) => (l.as_str(), r.as_str()),
                _ => return Ok(Value::Null),
            };
            regex_match(r, l, true).map(Value::Boolean)
        }
        BinaryOperator::PGRegexNotMatch => {
            let (l, r) = match (left, right) {
                (Value::String(l), Value::String(r)) => (l.as_str(), r.as_str()),
                _ => return Ok(Value::Null),
            };
            regex_match(r, l, false).map(|b| Value::Boolean(!b))
        }
        BinaryOperator::PGRegexNotIMatch => {
            let (l, r) = match (left, right) {
                (Value::String(l), Value::String(r)) => (l.as_str(), r.as_str()),
                _ => return Ok(Value::Null),
            };
            regex_match(r, l, true).map(|b| Value::Boolean(!b))
        }
        BinaryOperator::Arrow | BinaryOperator::LongArrow => {
            let serde_val = match left {
                Value::Json(j) => j.clone(),
                Value::String(s) => match serde_json::from_str(s) {
                    Ok(j) => j,
                    Err(_) => return Ok(Value::Null),
                },
                _ => return Ok(Value::Null),
            };
            let key = match right {
                Value::String(s) => s.as_str(),
                Value::Integer(i) => {
                    if let Some(target) = serde_val.get(*i as usize) {
                        return if matches!(op, BinaryOperator::LongArrow) {
                            match target {
                                serde_json::Value::String(s) => Ok(Value::String(s.clone())),
                                _ => Ok(Value::String(target.to_string())),
                            }
                        } else {
                            Ok(Value::Json(target.clone()))
                        };
                    } else {
                        return Ok(Value::Null);
                    }
                }
                _ => return Ok(Value::Null),
            };

            if let Some(target) = extract_json_path(&serde_val, key) {
                if matches!(op, BinaryOperator::LongArrow) {
                    match target {
                        serde_json::Value::String(s) => Ok(Value::String(s.clone())),
                        _ => Ok(Value::String(target.to_string())),
                    }
                } else {
                    Ok(Value::Json(target.clone()))
                }
            } else {
                Ok(Value::Null)
            }
        }
        _ => Err(H2Error::Execution(format!("Unsupported operator: {:?}", op))),
    }
}


fn evaluate_arithmetic_op(left: &Value, op: &BinaryOperator, right: &Value) -> H2Result<Value> {
    if left.is_null() || right.is_null() {
        return Ok(Value::Null);
    }
    match (left, right) {
        // Date / Timestamp + Interval
        (Value::Date(d), Value::Interval(iv)) if matches!(op, BinaryOperator::Plus) => {
            Ok(Value::Date(add_interval_to_date(*d, iv)))
        }
        (Value::Interval(iv), Value::Date(d)) if matches!(op, BinaryOperator::Plus) => {
            Ok(Value::Date(add_interval_to_date(*d, iv)))
        }
        (Value::Date(d), Value::Interval(iv)) if matches!(op, BinaryOperator::Minus) => {
            let neg_iv = IntervalValue::new(-iv.months, -iv.days, -iv.microseconds);
            Ok(Value::Date(add_interval_to_date(*d, &neg_iv)))
        }
        (Value::Timestamp(ts), Value::Interval(iv)) if matches!(op, BinaryOperator::Plus) => {
            Ok(Value::Timestamp(add_interval_to_datetime(*ts, iv)))
        }
        (Value::Interval(iv), Value::Timestamp(ts)) if matches!(op, BinaryOperator::Plus) => {
            Ok(Value::Timestamp(add_interval_to_datetime(*ts, iv)))
        }
        (Value::Timestamp(ts), Value::Interval(iv)) if matches!(op, BinaryOperator::Minus) => {
            let neg_iv = IntervalValue::new(-iv.months, -iv.days, -iv.microseconds);
            Ok(Value::Timestamp(add_interval_to_datetime(*ts, &neg_iv)))
        }
        (Value::String(s), Value::Interval(iv)) if matches!(op, BinaryOperator::Plus | BinaryOperator::Minus) => {
            if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
                let utc_dt = dt.with_timezone(&chrono::Utc);
                let res = if matches!(op, BinaryOperator::Plus) {
                    add_interval_to_datetime(utc_dt, iv)
                } else {
                    let neg_iv = IntervalValue::new(-iv.months, -iv.days, -iv.microseconds);
                    add_interval_to_datetime(utc_dt, &neg_iv)
                };
                Ok(Value::Timestamp(res))
            } else if let Ok(naive) = chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S") {
                let utc_dt = naive.and_utc();
                let res = if matches!(op, BinaryOperator::Plus) {
                    add_interval_to_datetime(utc_dt, iv)
                } else {
                    let neg_iv = IntervalValue::new(-iv.months, -iv.days, -iv.microseconds);
                    add_interval_to_datetime(utc_dt, &neg_iv)
                };
                Ok(Value::Timestamp(res))
            } else if let Ok(d) = chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d") {
                let res = if matches!(op, BinaryOperator::Plus) {
                    add_interval_to_date(d, iv)
                } else {
                    let neg_iv = IntervalValue::new(-iv.months, -iv.days, -iv.microseconds);
                    add_interval_to_date(d, &neg_iv)
                };
                Ok(Value::Date(res))
            } else {
                Err(H2Error::TypeError(format!("Cannot parse '{}' as date/time for INTERVAL operation", s)))
            }
        }
        (Value::Interval(a), Value::Interval(b)) => match op {
            BinaryOperator::Plus => Ok(Value::Interval(IntervalValue::new(a.months + b.months, a.days + b.days, a.microseconds + b.microseconds))),
            BinaryOperator::Minus => Ok(Value::Interval(IntervalValue::new(a.months - b.months, a.days - b.days, a.microseconds - b.microseconds))),
            _ => Err(H2Error::TypeError(format!("Cannot apply operator {:?} to INTERVAL", op))),
        },
        (Value::Date(d1), Value::Date(d2)) if matches!(op, BinaryOperator::Minus) => {
            let days = (*d1 - *d2).num_days() as i32;
            Ok(Value::Interval(IntervalValue::from_days(days)))
        }
        (Value::Timestamp(t1), Value::Timestamp(t2)) if matches!(op, BinaryOperator::Minus) => {
            let micro = (*t1 - *t2).num_microseconds().unwrap_or(0);
            Ok(Value::Interval(IntervalValue::new(0, 0, micro)))
        }
        // 文字列連結演算 (+ 演算子: SQL Server / 方言互換)
        (Value::String(l), Value::String(r)) if matches!(op, BinaryOperator::Plus) => {
            Ok(Value::String(format!("{}{}", l, r)))
        }
        (Value::String(l), r) if matches!(op, BinaryOperator::Plus) => {
            Ok(Value::String(format!("{}{}", l, r.to_string())))
        }
        (l, Value::String(r)) if matches!(op, BinaryOperator::Plus) => {
            Ok(Value::String(format!("{}{}", l.to_string(), r)))
        }
        // 数値演算
        (Value::Integer(l), Value::Integer(r)) => match op {
            BinaryOperator::Plus => Ok(Value::Integer(l.wrapping_add(*r))),
            BinaryOperator::Minus => Ok(Value::Integer(l.wrapping_sub(*r))),
            BinaryOperator::Multiply => Ok(Value::Integer(l.wrapping_mul(*r))),
            BinaryOperator::Divide => {
                if *r == 0 {
                    Err(H2Error::Execution("Division by zero".to_string()))
                } else {
                    Ok(Value::Integer(l / r))
                }
            }
            BinaryOperator::Modulo => {
                if *r == 0 {
                    Err(H2Error::Execution("Division by zero".to_string()))
                } else {
                    Ok(Value::Integer(l % r))
                }
            }
            _ => unreachable!(),
        },
        (Value::BigInt(l), Value::BigInt(r)) => match op {
            BinaryOperator::Plus => Ok(Value::BigInt(l.wrapping_add(*r))),
            BinaryOperator::Minus => Ok(Value::BigInt(l.wrapping_sub(*r))),
            BinaryOperator::Multiply => Ok(Value::BigInt(l.wrapping_mul(*r))),
            BinaryOperator::Divide => {
                if *r == 0 {
                    Err(H2Error::Execution("Division by zero".to_string()))
                } else {
                    Ok(Value::BigInt(l / r))
                }
            }
            BinaryOperator::Modulo => {
                if *r == 0 {
                    Err(H2Error::Execution("Division by zero".to_string()))
                } else {
                    Ok(Value::BigInt(l % r))
                }
            }
            _ => unreachable!(),
        },
        (Value::Double(l), Value::Double(r)) => match op {
            BinaryOperator::Plus => Ok(Value::Double(l + r)),
            BinaryOperator::Minus => Ok(Value::Double(l - r)),
            BinaryOperator::Multiply => Ok(Value::Double(l * r)),
            BinaryOperator::Divide => Ok(Value::Double(l / r)),
            BinaryOperator::Modulo => Ok(Value::Double(l % r)),
            _ => unreachable!(),
        },
        (Value::Decimal(l), Value::Decimal(r)) => match op {
            BinaryOperator::Plus => Ok(Value::Decimal(*l + *r)),
            BinaryOperator::Minus => Ok(Value::Decimal(*l - *r)),
            BinaryOperator::Multiply => Ok(Value::Decimal(*l * *r)),
            BinaryOperator::Divide => {
                if r.is_zero() {
                    Err(H2Error::Execution("Division by zero".to_string()))
                } else {
                    Ok(Value::Decimal(*l / *r))
                }
            }
            BinaryOperator::Modulo => {
                if r.is_zero() {
                    Err(H2Error::Execution("Division by zero".to_string()))
                } else {
                    Ok(Value::Decimal(*l % *r))
                }
            }
            _ => unreachable!(),
        },
        // 数値の型が混在している場合は i64 / f64 に格上げ
        (l, r) => {
            if matches!(op, BinaryOperator::Modulo) {
                if let (Some(li), Some(ri)) = (l.to_i64(), r.to_i64()) {
                    if ri == 0 {
                        return Err(H2Error::Execution("Division by zero".to_string()));
                    }
                    return Ok(Value::BigInt(li % ri));
                }
            }
            if let (Some(lf), Some(rf)) = (l.to_f64(), r.to_f64()) {
                match op {
                    BinaryOperator::Plus => Ok(Value::Double(lf + rf)),
                    BinaryOperator::Minus => Ok(Value::Double(lf - rf)),
                    BinaryOperator::Multiply => Ok(Value::Double(lf * rf)),
                    BinaryOperator::Divide => Ok(Value::Double(lf / rf)),
                    BinaryOperator::Modulo => Ok(Value::Double(lf % rf)),
                    _ => unreachable!(),
                }
            } else {
                Err(H2Error::TypeError(format!("Cannot apply arithmetic operator {:?} to {:?} and {:?}", op, left, right)))
            }
        }
    }
}

fn extract_json_path<'a>(json: &'a serde_json::Value, path: &str) -> Option<&'a serde_json::Value> {
    let clean_path = path.trim_start_matches('$').trim_start_matches('.');
    if clean_path.is_empty() {
        return Some(json);
    }
    let parts: Vec<&str> = clean_path.split('.').collect();
    let mut curr = json;
    for part in parts {
        if let Some(open_bracket) = part.find('[') {
            let key = &part[..open_bracket];
            if !key.is_empty() {
                curr = curr.get(key)?;
            }
            let close_bracket = part.find(']')?;
            let idx_str = &part[open_bracket + 1..close_bracket];
            let idx: usize = idx_str.parse().ok()?;
            curr = curr.get(idx)?;
        } else {
            curr = curr.get(part)?;
        }
    }
    Some(curr)
}

fn json_to_value(j: &serde_json::Value) -> Value {
    match j {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Bool(b) => Value::Boolean(*b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                if i >= i32::MIN as i64 && i <= i32::MAX as i64 {
                    Value::Integer(i as i32)
                } else {
                    Value::BigInt(i)
                }
            } else if let Some(f) = n.as_f64() {
                Value::Double(f)
            } else {
                Value::Null
            }
        }
        serde_json::Value::String(s) => Value::String(s.clone()),
        serde_json::Value::Array(_) | serde_json::Value::Object(_) => Value::Json(j.clone()),
    }
}

pub fn substring_impl(s: &str, from_val: &Value, for_val: Option<&Value>) -> H2Result<Value> {
    let from = from_val.to_i64().unwrap_or(1);
    let chars: Vec<char> = s.chars().collect();
    let start_idx = if from <= 0 { 0 } else { (from - 1) as usize };
    if start_idx >= chars.len() {
        return Ok(Value::String(String::new()));
    }
    let sub = if let Some(for_v) = for_val {
        let len = for_v.to_i64().unwrap_or(0);
        if len <= 0 {
            String::new()
        } else {
            let end_idx = (start_idx + len as usize).min(chars.len());
            chars[start_idx..end_idx].iter().collect()
        }
    } else {
        chars[start_idx..].iter().collect()
    };
    Ok(Value::String(sub))
}

pub fn initcap(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut capitalize_next = true;
    for c in s.chars() {
        if c.is_alphanumeric() {
            if capitalize_next {
                result.extend(c.to_uppercase());
                capitalize_next = false;
            } else {
                result.extend(c.to_lowercase());
            }
        } else {
            result.push(c);
            capitalize_next = true;
        }
    }
    result
}

pub fn translate(s: &str, from: &str, to: &str) -> String {
    let from_chars: Vec<char> = from.chars().collect();
    let to_chars: Vec<char> = to.chars().collect();
    let mut map = std::collections::HashMap::new();
    let mut delete_set = std::collections::HashSet::new();

    for (i, &fc) in from_chars.iter().enumerate() {
        if !map.contains_key(&fc) && !delete_set.contains(&fc) {
            if i < to_chars.len() {
                map.insert(fc, to_chars[i]);
            } else {
                delete_set.insert(fc);
            }
        }
    }

    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if delete_set.contains(&c) {
            continue;
        }
        if let Some(&rep) = map.get(&c) {
            out.push(rep);
        } else {
            out.push(c);
        }
    }
    out
}

pub fn regex_match(pattern: &str, text: &str, case_insensitive: bool) -> H2Result<bool> {
    let mut builder = regex::RegexBuilder::new(pattern);
    builder.case_insensitive(case_insensitive);
    match builder.build() {
        Ok(re) => Ok(re.is_match(text)),
        Err(e) => Err(H2Error::Execution(format!("Invalid regex pattern '{}': {}", pattern, e))),
    }
}

pub fn extract_field(val: &Value, field: &str) -> H2Result<Value> {
    use chrono::Timelike;
    let field = field.to_uppercase();
    match val {
        Value::Date(d) => match field.as_str() {
            "YEAR" => Ok(Value::Integer(d.year())),
            "MONTH" => Ok(Value::Integer(d.month() as i32)),
            "DAY" => Ok(Value::Integer(d.day() as i32)),
            "DOW" => Ok(Value::Integer(d.weekday().num_days_from_sunday() as i32)),
            "DOY" => Ok(Value::Integer(d.ordinal() as i32)),
            "EPOCH" => Ok(Value::BigInt(d.and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp())),
            _ => Err(H2Error::Execution(format!("Cannot extract {} from DATE", field))),
        },
        Value::Time(t) => match field.as_str() {
            "HOUR" => Ok(Value::Integer(t.hour() as i32)),
            "MINUTE" => Ok(Value::Integer(t.minute() as i32)),
            "SECOND" => Ok(Value::Integer(t.second() as i32)),
            _ => Err(H2Error::Execution(format!("Cannot extract {} from TIME", field))),
        },
        Value::Timestamp(ts) => match field.as_str() {
            "YEAR" => Ok(Value::Integer(ts.year())),
            "MONTH" => Ok(Value::Integer(ts.month() as i32)),
            "DAY" => Ok(Value::Integer(ts.day() as i32)),
            "HOUR" => Ok(Value::Integer(ts.hour() as i32)),
            "MINUTE" => Ok(Value::Integer(ts.minute() as i32)),
            "SECOND" => Ok(Value::Integer(ts.second() as i32)),
            "DOW" => Ok(Value::Integer(ts.weekday().num_days_from_sunday() as i32)),
            "DOY" => Ok(Value::Integer(ts.ordinal() as i32)),
            "EPOCH" => Ok(Value::BigInt(ts.timestamp())),
            _ => Err(H2Error::Execution(format!("Cannot extract {} from TIMESTAMP", field))),
        },
        Value::String(s) => {
            if let Ok(ts) = chrono::DateTime::parse_from_rfc3339(s) {
                extract_field(&Value::Timestamp(ts.with_timezone(&chrono::Utc)), &field)
            } else if let Ok(d) = chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d") {
                extract_field(&Value::Date(d), &field)
            } else {
                Err(H2Error::Execution(format!("Cannot extract field from string '{}'", s)))
            }
        }
        Value::Interval(iv) => match field.as_str() {
            "YEAR" => Ok(Value::Integer(iv.months / 12)),
            "MONTH" => Ok(Value::Integer(iv.months % 12)),
            "DAY" => Ok(Value::Integer(iv.days)),
            "HOUR" => Ok(Value::Integer((iv.microseconds / 3_600_000_000) as i32)),
            "MINUTE" => Ok(Value::Integer(((iv.microseconds / 60_000_000) % 60) as i32)),
            "SECOND" => Ok(Value::Integer(((iv.microseconds / 1_000_000) % 60) as i32)),
            _ => Err(H2Error::Execution(format!("Cannot extract {} from INTERVAL", field))),
        },
        Value::Null => Ok(Value::Null),
        _ => Err(H2Error::Execution(format!("Cannot extract {} from {:?}", field, val))),
    }
}

pub fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0) {
                29
            } else {
                28
            }
        }
        _ => 30,
    }
}

pub fn add_interval_to_datetime(dt: chrono::DateTime<chrono::Utc>, iv: &IntervalValue) -> chrono::DateTime<chrono::Utc> {
    let mut year = dt.year();
    let mut month = dt.month() as i32 + iv.months;
    while month > 12 {
        year += 1;
        month -= 12;
    }
    while month < 1 {
        year -= 1;
        month += 12;
    }
    let day = dt.day().min(days_in_month(year, month as u32));
    let naive_date = chrono::NaiveDate::from_ymd_opt(year, month as u32, day).unwrap_or(dt.date_naive());
    let naive_time = dt.time();
    let naive_dt = chrono::NaiveDateTime::new(naive_date, naive_time);
    let dt_utc = chrono::DateTime::<chrono::Utc>::from_naive_utc_and_offset(naive_dt, chrono::Utc);
    dt_utc + chrono::Duration::days(iv.days as i64) + chrono::Duration::microseconds(iv.microseconds)
}

pub fn add_interval_to_date(d: chrono::NaiveDate, iv: &IntervalValue) -> chrono::NaiveDate {
    let mut year = d.year();
    let mut month = d.month() as i32 + iv.months;
    while month > 12 {
        year += 1;
        month -= 12;
    }
    while month < 1 {
        year -= 1;
        month += 12;
    }
    let day = d.day().min(days_in_month(year, month as u32));
    let base = chrono::NaiveDate::from_ymd_opt(year, month as u32, day).unwrap_or(d);
    base + chrono::Duration::days(iv.days as i64)
}


