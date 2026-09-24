use rust_decimal::Decimal;
use sqlparser::ast::{BinaryOperator, Expr as SqlExpr, Value as SqlValue};
use std::str::FromStr;

use h2_types::{H2Error, H2Result, Value};
use crate::catalog::TableDef;
use crate::row::Row;

pub fn evaluate_expr(expr: &SqlExpr, table: &TableDef, row: &Row) -> H2Result<Value> {
    match expr {
        SqlExpr::Value(val) => evaluate_sql_value(val),
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
        _ => Err(H2Error::Execution(format!("Unsupported expression: {:?}", expr))),
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

fn evaluate_binary_op(left: &Value, op: &BinaryOperator, right: &Value) -> H2Result<Value> {
    match op {
        BinaryOperator::Eq => Ok(Value::Boolean(left == right)),
        BinaryOperator::NotEq => Ok(Value::Boolean(left != right)),
        BinaryOperator::Lt => Ok(Value::Boolean(left < right)),
        BinaryOperator::LtEq => Ok(Value::Boolean(left <= right)),
        BinaryOperator::Gt => Ok(Value::Boolean(left > right)),
        BinaryOperator::GtEq => Ok(Value::Boolean(left >= right)),
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
