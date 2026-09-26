use std::collections::HashMap;
use sqlparser::ast::Expr;
use h2_types::{H2Error, H2Result, Value};
use crate::expression::{evaluate_expr_context, RowContext};
use crate::row::Row;
use super::query::value_to_sql_expr;

pub(crate) fn collect_window_functions(expr: &Expr, funcs: &mut Vec<sqlparser::ast::Function>) {
    match expr {
        Expr::Function(func) => {
            if func.over.is_some() {
                if !funcs.iter().any(|f| f.to_string() == func.to_string()) {
                    funcs.push(func.clone());
                }
            }
        }
        Expr::BinaryOp { left, right, .. } => {
            collect_window_functions(left, funcs);
            collect_window_functions(right, funcs);
        }
        Expr::UnaryOp { expr, .. } => {
            collect_window_functions(expr, funcs);
        }
        Expr::Nested(e) => {
            collect_window_functions(e, funcs);
        }
        Expr::Cast { expr: inner, .. } => {
            collect_window_functions(inner, funcs);
        }
        Expr::Case { conditions, results, else_result, operand } => {
            if let Some(op) = operand {
                collect_window_functions(op, funcs);
            }
            for c in conditions {
                collect_window_functions(c, funcs);
            }
            for r in results {
                collect_window_functions(r, funcs);
            }
            if let Some(el) = else_result {
                collect_window_functions(el, funcs);
            }
        }
        Expr::Tuple(exprs) => {
            for e in exprs {
                collect_window_functions(e, funcs);
            }
        }
        _ => {}
    }
}

pub(crate) fn replace_window_function(expr: &Expr, window_func_str: &str, replacement: &Value) -> Expr {
    match expr {
        Expr::Function(func) => {
            if func.over.is_some() && func.to_string() == window_func_str {
                value_to_sql_expr(replacement.clone())
            } else {
                expr.clone()
            }
        }
        Expr::BinaryOp { left, op, right } => Expr::BinaryOp {
            left: Box::new(replace_window_function(left, window_func_str, replacement)),
            op: op.clone(),
            right: Box::new(replace_window_function(right, window_func_str, replacement)),
        },
        Expr::UnaryOp { op, expr: inner } => Expr::UnaryOp {
            op: op.clone(),
            expr: Box::new(replace_window_function(inner, window_func_str, replacement)),
        },
        Expr::Nested(e) => Expr::Nested(Box::new(replace_window_function(e, window_func_str, replacement))),
        Expr::Tuple(exprs) => Expr::Tuple(exprs.iter().map(|e| replace_window_function(e, window_func_str, replacement)).collect()),
        Expr::Cast { expr: inner, data_type, format, kind } => Expr::Cast {
            expr: Box::new(replace_window_function(inner, window_func_str, replacement)),
            data_type: data_type.clone(),
            format: format.clone(),
            kind: kind.clone(),
        },
        Expr::Case { operand, conditions, results, else_result } => Expr::Case {
            operand: operand.as_ref().map(|op| Box::new(replace_window_function(op, window_func_str, replacement))),
            conditions: conditions.iter().map(|c| replace_window_function(c, window_func_str, replacement)).collect(),
            results: results.iter().map(|r| replace_window_function(r, window_func_str, replacement)).collect(),
            else_result: else_result.as_ref().map(|el| Box::new(replace_window_function(el, window_func_str, replacement))),
        },
        _ => expr.clone(),
    }
}

pub(crate) fn get_func_arg_expr(arg: &sqlparser::ast::FunctionArg) -> H2Result<&Expr> {
    match arg {
        sqlparser::ast::FunctionArg::Unnamed(sqlparser::ast::FunctionArgExpr::Expr(e)) => Ok(e),
        _ => Err(H2Error::Execution("Unsupported function argument in window function".to_string())),
    }
}

pub(crate) fn compute_window_functions(
    funcs: &[sqlparser::ast::Function],
    rows: &[Row],
    ctx: &RowContext,
) -> H2Result<HashMap<String, Vec<Value>>> {
    let mut results = HashMap::new();

    for func in funcs {
        let func_name = func.name.to_string().to_uppercase();
        let spec = match &func.over {
            Some(sqlparser::ast::WindowType::WindowSpec(spec)) => spec,
            _ => return Err(H2Error::Execution("Named window specifications not supported yet".to_string())),
        };

        let args = match &func.args {
            sqlparser::ast::FunctionArguments::List(arg_list) => &arg_list.args,
            sqlparser::ast::FunctionArguments::None => &Vec::new(),
            _ => return Err(H2Error::Execution("Invalid function args".to_string())),
        };

        let partition_by = &spec.partition_by;
        let order_by = &spec.order_by;

        // 1. パーティション分割
        let mut partitions: Vec<(Vec<Value>, Vec<usize>)> = Vec::new();
        for (i, row) in rows.iter().enumerate() {
            let mut key = Vec::with_capacity(partition_by.len());
            for expr in partition_by {
                key.push(evaluate_expr_context(expr, ctx, row).unwrap_or(Value::Null));
            }
            if let Some(pos) = partitions.iter().position(|(k, _)| k == &key) {
                partitions[pos].1.push(i);
            } else {
                partitions.push((key, vec![i]));
            }
        }

        let mut func_results = vec![Value::Null; rows.len()];

        // 2. 各パーティション内でソート＆値割り当て
        for (_part_key, mut group_indices) in partitions {
            if !order_by.is_empty() {
                group_indices.sort_by(|&a_idx, &b_idx| {
                    let row_a = &rows[a_idx];
                    let row_b = &rows[b_idx];
                    for order_expr in order_by {
                        let val_a = evaluate_expr_context(&order_expr.expr, ctx, row_a).unwrap_or(Value::Null);
                        let val_b = evaluate_expr_context(&order_expr.expr, ctx, row_b).unwrap_or(Value::Null);
                        let mut ord = val_a.partial_cmp(&val_b).unwrap_or(std::cmp::Ordering::Equal);
                        let is_asc = order_expr.asc.unwrap_or(true);
                        if !is_asc {
                            ord = ord.reverse();
                        }
                        if ord != std::cmp::Ordering::Equal {
                            return ord;
                        }
                    }
                    std::cmp::Ordering::Equal
                });
            }

            match func_name.as_str() {
                "ROW_NUMBER" => {
                    for (rank, &row_idx) in group_indices.iter().enumerate() {
                        func_results[row_idx] = Value::BigInt((rank + 1) as i64);
                    }
                }
                "RANK" => {
                    let mut current_rank = 1;
                    for i in 0..group_indices.len() {
                        let row_idx = group_indices[i];
                        if i > 0 {
                            let prev_row_idx = group_indices[i - 1];
                            let is_same = if order_by.is_empty() {
                                true
                            } else {
                                order_by.iter().all(|order_expr| {
                                    let val_curr = evaluate_expr_context(&order_expr.expr, ctx, &rows[row_idx]).unwrap_or(Value::Null);
                                    let val_prev = evaluate_expr_context(&order_expr.expr, ctx, &rows[prev_row_idx]).unwrap_or(Value::Null);
                                    val_curr == val_prev
                                })
                            };
                            if !is_same {
                                current_rank = (i + 1) as i64;
                            }
                        }
                        func_results[row_idx] = Value::BigInt(current_rank);
                    }
                }
                "DENSE_RANK" => {
                    let mut current_dense_rank = 1;
                    for i in 0..group_indices.len() {
                        let row_idx = group_indices[i];
                        if i > 0 {
                            let prev_row_idx = group_indices[i - 1];
                            let is_same = if order_by.is_empty() {
                                true
                            } else {
                                order_by.iter().all(|order_expr| {
                                    let val_curr = evaluate_expr_context(&order_expr.expr, ctx, &rows[row_idx]).unwrap_or(Value::Null);
                                    let val_prev = evaluate_expr_context(&order_expr.expr, ctx, &rows[prev_row_idx]).unwrap_or(Value::Null);
                                    val_curr == val_prev
                                })
                            };
                            if !is_same {
                                current_dense_rank += 1;
                            }
                        }
                        func_results[row_idx] = Value::BigInt(current_dense_rank);
                    }
                }
                "LEAD" => {
                    let arg0_expr = if !args.is_empty() {
                        get_func_arg_expr(&args[0])?
                    } else {
                        return Err(H2Error::Execution("LEAD requires at least 1 argument".to_string()));
                    };

                    let offset: usize = if args.len() > 1 {
                        let off_expr = get_func_arg_expr(&args[1])?;
                        let dummy_row = Row::new(vec![]);
                        match evaluate_expr_context(off_expr, ctx, &dummy_row)? {
                            Value::TinyInt(n) => n.max(0) as usize,
                            Value::SmallInt(n) => n.max(0) as usize,
                            Value::Integer(n) => n.max(0) as usize,
                            Value::BigInt(n) => n.max(0) as usize,
                            _ => return Err(H2Error::Execution("LEAD offset must be an integer".to_string())),
                        }
                    } else {
                        1
                    };

                    let default_val = if args.len() > 2 {
                        let def_expr = get_func_arg_expr(&args[2])?;
                        let dummy_row = Row::new(vec![]);
                        evaluate_expr_context(def_expr, ctx, &dummy_row)?
                    } else {
                        Value::Null
                    };

                    for pos in 0..group_indices.len() {
                        let row_idx = group_indices[pos];
                        let target_pos = pos + offset;
                        if target_pos < group_indices.len() {
                            let target_row_idx = group_indices[target_pos];
                            func_results[row_idx] = evaluate_expr_context(arg0_expr, ctx, &rows[target_row_idx])?;
                        } else {
                            func_results[row_idx] = default_val.clone();
                        }
                    }
                }
                "LAG" => {
                    let arg0_expr = if !args.is_empty() {
                        get_func_arg_expr(&args[0])?
                    } else {
                        return Err(H2Error::Execution("LAG requires at least 1 argument".to_string()));
                    };

                    let offset: usize = if args.len() > 1 {
                        let off_expr = get_func_arg_expr(&args[1])?;
                        let dummy_row = Row::new(vec![]);
                        match evaluate_expr_context(off_expr, ctx, &dummy_row)? {
                            Value::TinyInt(n) => n.max(0) as usize,
                            Value::SmallInt(n) => n.max(0) as usize,
                            Value::Integer(n) => n.max(0) as usize,
                            Value::BigInt(n) => n.max(0) as usize,
                            _ => return Err(H2Error::Execution("LAG offset must be an integer".to_string())),
                        }
                    } else {
                        1
                    };

                    let default_val = if args.len() > 2 {
                        let def_expr = get_func_arg_expr(&args[2])?;
                        let dummy_row = Row::new(vec![]);
                        evaluate_expr_context(def_expr, ctx, &dummy_row)?
                    } else {
                        Value::Null
                    };

                    for pos in 0..group_indices.len() {
                        let row_idx = group_indices[pos];
                        if pos >= offset {
                            let target_row_idx = group_indices[pos - offset];
                            func_results[row_idx] = evaluate_expr_context(arg0_expr, ctx, &rows[target_row_idx])?;
                        } else {
                            func_results[row_idx] = default_val.clone();
                        }
                    }
                }
                "FIRST_VALUE" => {
                    let arg0_expr = if !args.is_empty() {
                        get_func_arg_expr(&args[0])?
                    } else {
                        return Err(H2Error::Execution("FIRST_VALUE requires 1 argument".to_string()));
                    };

                    if !group_indices.is_empty() {
                        let first_row_idx = group_indices[0];
                        let val = evaluate_expr_context(arg0_expr, ctx, &rows[first_row_idx])?;
                        for &row_idx in &group_indices {
                            func_results[row_idx] = val.clone();
                        }
                    }
                }
                "LAST_VALUE" => {
                    let arg0_expr = if !args.is_empty() {
                        get_func_arg_expr(&args[0])?
                    } else {
                        return Err(H2Error::Execution("LAST_VALUE requires 1 argument".to_string()));
                    };

                    if !group_indices.is_empty() {
                        let last_row_idx = group_indices[group_indices.len() - 1];
                        let val = evaluate_expr_context(arg0_expr, ctx, &rows[last_row_idx])?;
                        for &row_idx in &group_indices {
                            func_results[row_idx] = val.clone();
                        }
                    }
                }
                "NTILE" => {
                    let arg0_expr = if !args.is_empty() {
                        get_func_arg_expr(&args[0])?
                    } else {
                        return Err(H2Error::Execution("NTILE requires 1 argument".to_string()));
                    };

                    let dummy_row = Row::new(vec![]);
                    let buckets = match evaluate_expr_context(arg0_expr, ctx, &dummy_row)? {
                        Value::TinyInt(n) => n as usize,
                        Value::SmallInt(n) => n as usize,
                        Value::Integer(n) => n as usize,
                        Value::BigInt(n) => n as usize,
                        _ => return Err(H2Error::Execution("NTILE argument must be an integer".to_string())),
                    };

                    if buckets == 0 {
                        return Err(H2Error::Execution("NTILE argument must be greater than 0".to_string()));
                    }

                    let n = group_indices.len();
                    if n > 0 {
                        let base_size = n / buckets;
                        let remainder = n % buckets;

                        for pos in 0..n {
                            let row_idx = group_indices[pos];
                            let bucket = if base_size == 0 {
                                pos + 1
                            } else if pos < remainder * (base_size + 1) {
                                (pos / (base_size + 1)) + 1
                            } else {
                                let pos_after = pos - remainder * (base_size + 1);
                                remainder + (pos_after / base_size) + 1
                            };
                            func_results[row_idx] = Value::BigInt(bucket as i64);
                        }
                    }
                }
                _ => return Err(H2Error::Execution(format!("Unsupported window function: {}", func_name))),
            }
        }

        results.insert(func.to_string(), func_results);
    }

    Ok(results)
}



