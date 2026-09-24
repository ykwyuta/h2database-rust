use rust_decimal::Decimal;
use sqlparser::ast::{BinaryOperator, Expr as SqlExpr, FunctionArg, FunctionArgExpr, Value as SqlValue};
use std::str::FromStr;

use h2_types::{H2Error, H2Result, Value};
use crate::catalog::TableDef;
use crate::fts::tokenizer::{get_tokenizer, TokenizerKind};
use crate::row::Row;

pub fn evaluate_expr(expr: &SqlExpr, table: &TableDef, row: &Row) -> H2Result<Value> {
    match expr {
        SqlExpr::Value(val) => evaluate_sql_value(val),
        SqlExpr::UnaryOp { op, expr } => {
            let inner = evaluate_expr(expr, table, row)?;
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
            let col_idx = table.column_index(&ident.value).ok_or_else(|| {
                H2Error::Execution(format!("Column '{}' not found in table '{}'", ident.value, table.name))
            })?;
            Ok(row.get(col_idx).cloned().unwrap_or(Value::Null))
        }
        SqlExpr::BinaryOp { left, op, right } => {
            let l_val = evaluate_expr(left, table, row)?;
            let r_val = evaluate_expr(right, table, row)?;
            evaluate_binary_op(&l_val, op, &r_val)
        }
        // LIKE 演算子
        SqlExpr::Like { expr, pattern, .. } => {
            let val = evaluate_expr(expr, table, row)?;
            let pat = evaluate_expr(pattern, table, row)?;
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

                let col_val = evaluate_func_arg(&args[0], table, row)?;
                let query_val = evaluate_func_arg(&args[1], table, row)?;

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
            } else {
                Err(H2Error::Execution(format!("Unsupported function: {}", func_name)))
            }
        }
        _ => Err(H2Error::Execution(format!("Unsupported expression: {:?}", expr))),
    }
}

fn evaluate_func_arg(arg: &FunctionArg, table: &TableDef, row: &Row) -> H2Result<Value> {
    match arg {
        FunctionArg::Unnamed(FunctionArgExpr::Expr(expr)) => evaluate_expr(expr, table, row),
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

fn evaluate_binary_op(left: &Value, op: &BinaryOperator, right: &Value) -> H2Result<Value> {
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
        _ => Err(H2Error::Execution(format!("Unsupported operator: {:?}", op))),
    }
}
