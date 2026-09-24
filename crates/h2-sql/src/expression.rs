use rust_decimal::Decimal;
use sqlparser::ast::{BinaryOperator, Expr as SqlExpr, FunctionArg, FunctionArgExpr, Value as SqlValue};
use std::str::FromStr;

use h2_types::{H2Error, H2Result, Value};
use crate::catalog::TableDef;
use crate::fts::tokenizer::{get_tokenizer, TokenizerKind};
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
}

impl RowContext {
    pub fn new() -> Self {
        Self { columns: Vec::new() }
    }

    pub fn from_table_def(table: &TableDef, alias: Option<&str>) -> Self {
        let columns = table.columns.iter().enumerate().map(|(i, col)| ColumnBinding {
            table_name: Some(table.name.clone()),
            table_alias: alias.map(|s| s.to_string()),
            column_name: col.name.clone(),
            index: i,
        }).collect();
        Self { columns }
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
        SqlExpr::Value(val) => evaluate_sql_value(val),
        SqlExpr::Nested(inner) => evaluate_expr_context(inner, ctx, row),
        SqlExpr::IsNull(inner) => {
            let v = evaluate_expr_context(inner, ctx, row)?;
            Ok(Value::Boolean(matches!(v, Value::Null)))
        }
        SqlExpr::IsNotNull(inner) => {
            let v = evaluate_expr_context(inner, ctx, row)?;
            Ok(Value::Boolean(!matches!(v, Value::Null)))
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
            let col_idx = ctx.resolve_column(None, &ident.value).ok_or_else(|| {
                H2Error::Execution(format!("Column '{}' not found", ident.value))
            })?;
            Ok(row.get(col_idx).cloned().unwrap_or(Value::Null))
        }
        SqlExpr::CompoundIdentifier(idents) => {
            if idents.len() == 2 {
                let tbl = &idents[0].value;
                let col = &idents[1].value;
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
        SqlExpr::Like { expr, pattern, .. } => {
            let val = evaluate_expr_context(expr, ctx, row)?;
            let pat = evaluate_expr_context(pattern, ctx, row)?;
            match (&val, &pat) {
                (Value::String(s), Value::String(p)) => {
                    let matched = if p.starts_with('%') && p.ends_with('%') {
                        let substr = &p[1..p.len() - 1];
                        s.contains(substr)
                    } else if p.starts_with('%') {
                        s.ends_with(&p[1..])
                    } else if p.ends_with('%') {
                        s.starts_with(&p[..p.len() - 1])
                    } else {
                        s == p
                    };
                    Ok(Value::Boolean(matched))
                }
                _ => Ok(Value::Boolean(false)),
            }
        }
        // 全文検索関数: FT_SEARCH(column, 'keyword') または FT_SEARCH_MORPH(column, 'keyword')
        SqlExpr::Function(func) => {
            let func_name = func.name.to_string().to_uppercase();
            if func_name == "FT_SEARCH" || func_name == "FT_SEARCH_MORPH" {
                let kind = if func_name == "FT_SEARCH_MORPH" {
                    TokenizerKind::Morph
                } else {
                    TokenizerKind::NGram
                };

                let args = match &func.args {
                    sqlparser::ast::FunctionArguments::List(arg_list) => &arg_list.args,
                    _ => return Err(H2Error::Execution("Invalid function args".to_string())),
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

                        // クエリの全トークンがドキュメントに含まれているか (AND判定)
                        let matched = !query_tokens.is_empty()
                            && query_tokens.iter().all(|q_tok| doc_tokens.contains(q_tok));
                        Ok(Value::Boolean(matched))
                    }
                    _ => Ok(Value::Boolean(false)),
                }
            } else if func_name == "JSON_EXTRACT" || func_name == "JSON_VALUE" {
                let args = match &func.args {
                    sqlparser::ast::FunctionArguments::List(arg_list) => &arg_list.args,
                    _ => return Err(H2Error::Execution("Invalid function args".to_string())),
                };
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
        BinaryOperator::Plus | BinaryOperator::Minus | BinaryOperator::Multiply | BinaryOperator::Divide => {
            evaluate_arithmetic_op(left, op, right)
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
            _ => unreachable!(),
        },
        (Value::Double(l), Value::Double(r)) => match op {
            BinaryOperator::Plus => Ok(Value::Double(l + r)),
            BinaryOperator::Minus => Ok(Value::Double(l - r)),
            BinaryOperator::Multiply => Ok(Value::Double(l * r)),
            BinaryOperator::Divide => Ok(Value::Double(l / r)),
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
            _ => unreachable!(),
        },
        // 数値の型が混在している場合は i64 / f64 に格上げ
        (l, r) => {
            if let (Some(lf), Some(rf)) = (l.to_f64(), r.to_f64()) {
                match op {
                    BinaryOperator::Plus => Ok(Value::Double(lf + rf)),
                    BinaryOperator::Minus => Ok(Value::Double(lf - rf)),
                    BinaryOperator::Multiply => Ok(Value::Double(lf * rf)),
                    BinaryOperator::Divide => Ok(Value::Double(lf / rf)),
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


