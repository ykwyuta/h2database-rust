use std::collections::HashMap;
use std::sync::Arc;
use sqlparser::ast::{
    BinaryOperator,
    Expr, FunctionArg, FunctionArgExpr, GroupByExpr, JoinConstraint, JoinOperator, Query, SelectItem, SetExpr, Statement, TableFactor,
};
use h2_mvstore::Transaction;
use h2_types::{H2Error, H2Result, Value};
use crate::catalog::{ColumnDef, TableDef};
use crate::expression::{
    evaluate_expr_context, evaluate_literal_or_unary, ColumnBinding, RowContext,
};
use crate::row::Row;
use super::*;

impl SQLEngine {
    pub(crate) fn parse_aggregate_ops(
        projection: &[SelectItem],
        ctx: &RowContext,
    ) -> Option<(Vec<String>, Vec<crate::vectorized::VectorAggregateOp>)> {
        let mut ops = Vec::new();
        let mut col_names = Vec::new();

        for item in projection {
            col_names.push(get_select_item_name(item));
            let expr = match item {
                SelectItem::UnnamedExpr(e) | SelectItem::ExprWithAlias { expr: e, .. } => e,
                _ => return None,
            };
            match expr {
                Expr::Function(func) => {
                    let fname = func.name.to_string().to_uppercase();
                    let args = match &func.args {
                        sqlparser::ast::FunctionArguments::List(arg_list) => &arg_list.args,
                        sqlparser::ast::FunctionArguments::None => &Vec::new(),
                        _ => return None,
                    };
                    if args.is_empty() && fname == "COUNT" {
                        ops.push(crate::vectorized::VectorAggregateOp::CountStar);
                    } else if args.len() == 1 {
                        let arg_expr = match &args[0] {
                            FunctionArg::Unnamed(FunctionArgExpr::Expr(e)) => e,
                            FunctionArg::Unnamed(FunctionArgExpr::Wildcard) if fname == "COUNT" => {
                                ops.push(crate::vectorized::VectorAggregateOp::CountStar);
                                continue;
                            }
                            _ => return None,
                        };
                        if let Expr::Identifier(ident) = arg_expr {
                            let col_name = ident.value.to_lowercase();
                            if let Some(col_idx) = ctx.resolve_column(None, &col_name) {
                                match fname.as_str() {
                                    "COUNT" => ops.push(crate::vectorized::VectorAggregateOp::Count(col_idx)),
                                    "SUM" => ops.push(crate::vectorized::VectorAggregateOp::Sum(col_idx)),
                                    "AVG" => ops.push(crate::vectorized::VectorAggregateOp::Avg(col_idx)),
                                    "MIN" => ops.push(crate::vectorized::VectorAggregateOp::Min(col_idx)),
                                    "MAX" => ops.push(crate::vectorized::VectorAggregateOp::Max(col_idx)),
                                    _ => return None,
                                }
                            } else {
                                return None;
                            }
                        } else {
                            return None;
                        }
                    } else {
                        return None;
                    }
                }
                _ => return None,
            }
        }
        Some((col_names, ops))
    }

    pub(crate) fn build_pushdown_aggregate_result(
        ops: &[crate::vectorized::VectorAggregateOp],
        col_names: &[String],
        counts: &[i64],
        sums: &[f64],
        mins: &[f64],
        maxs: &[f64],
        min_ints: &[i64],
        max_ints: &[i64],
        table_def: &TableDef,
    ) -> ExecutionResult {
        let mut row_values = Vec::with_capacity(ops.len());
        for (i, op) in ops.iter().enumerate() {
            match op {
                crate::vectorized::VectorAggregateOp::CountStar | crate::vectorized::VectorAggregateOp::Count(_) => {
                    row_values.push(Value::BigInt(counts[i]));
                }
                crate::vectorized::VectorAggregateOp::Sum(_) => {
                    if counts[i] == 0 {
                        row_values.push(Value::Null);
                    } else {
                        row_values.push(Value::BigInt(sums[i] as i64));
                    }
                }
                crate::vectorized::VectorAggregateOp::Avg(_) => {
                    if counts[i] == 0 {
                        row_values.push(Value::Null);
                    } else {
                        row_values.push(Value::Double(sums[i] / counts[i] as f64));
                    }
                }
                crate::vectorized::VectorAggregateOp::Min(col) => {
                    if counts[i] == 0 {
                        row_values.push(Value::Null);
                    } else {
                        match table_def.columns.get(*col).map(|c| &c.data_type) {
                            Some(h2_types::DataType::TinyInt | h2_types::DataType::SmallInt | h2_types::DataType::Integer | h2_types::DataType::BigInt) => {
                                row_values.push(Value::BigInt(min_ints[i]));
                            }
                            _ => {
                                row_values.push(Value::Double(mins[i]));
                            }
                        }
                    }
                }
                crate::vectorized::VectorAggregateOp::Max(col) => {
                    if counts[i] == 0 {
                        row_values.push(Value::Null);
                    } else {
                        match table_def.columns.get(*col).map(|c| &c.data_type) {
                            Some(h2_types::DataType::TinyInt | h2_types::DataType::SmallInt | h2_types::DataType::Integer | h2_types::DataType::BigInt) => {
                                row_values.push(Value::BigInt(max_ints[i]));
                            }
                            _ => {
                                row_values.push(Value::Double(maxs[i]));
                            }
                        }
                    }
                }
            }
        }

        ExecutionResult::Query {
            columns: col_names.to_vec(),
            rows: vec![Row::new(row_values)],
        }
    }

    pub(crate) fn try_execute_vectorized_aggregate(
        &self,
        projection: &[SelectItem],
        filtered_rows: &[Row],
        base_table_def: &TableDef,
        ctx: &RowContext,
    ) -> Option<(Vec<String>, Vec<Row>)> {
        let (col_names, ops) = Self::parse_aggregate_ops(projection, ctx)?;

        let current_mode = self.execution_mode();
        if current_mode == "vectorized" {
            let schema = crate::vectorized::create_arrow_schema(&base_table_def.columns);
            let batch = crate::vectorized::rows_to_record_batch(&schema, filtered_rows).ok()?;
            let chunk = crate::vectorized::VectorChunk::new(batch);
            let mem_op = Box::new(crate::vectorized::MemoryBatchOperator::single(chunk));
            let mut agg_op = crate::vectorized::VectorizedAggregate::new(mem_op, ops);
            let agg_row = agg_op.execute_aggregate().ok()?;
            Some((col_names, vec![agg_row]))
        } else {
            let agg_row = execute_fast_row_aggregate(&ops, filtered_rows);
            Some((col_names, vec![agg_row]))
        }
    }

    pub(crate) fn is_join_match(
    l: &Row,
    r: &Row,
    combined: &Row,
    constraint: Option<&JoinConstraint>,
    ctx: &RowContext,
    base_idx: usize,
    join_def: &TableDef,
) -> H2Result<bool> {
    match constraint {
        Some(JoinConstraint::On(on_expr)) => {
            match evaluate_expr_context(on_expr, ctx, combined)? {
                Value::Boolean(true) => Ok(true),
                _ => Ok(false),
            }
        }
        Some(JoinConstraint::Using(idents)) => {
            for ident in idents {
                let col_name = &ident.value;
                let l_idx = ctx.columns[..base_idx]
                    .iter()
                    .position(|cb| cb.column_name.eq_ignore_ascii_case(col_name))
                    .ok_or_else(|| H2Error::Execution(format!("Column '{}' in USING clause not found in left side", col_name)))?;
                let r_idx = join_def.column_index(col_name)
                    .ok_or_else(|| H2Error::Execution(format!("Column '{}' in USING clause not found in right side", col_name)))?;
                let val_l = l.get(l_idx).unwrap_or(&Value::Null);
                let val_r = r.get(r_idx).unwrap_or(&Value::Null);
                if val_l.is_null() || val_r.is_null() || val_l != val_r {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        Some(JoinConstraint::Natural) => {
            for r_col in &join_def.columns {
                if let Some(l_idx) = ctx.columns[..base_idx]
                    .iter()
                    .position(|cb| cb.column_name.eq_ignore_ascii_case(&r_col.name))
                {
                    let r_idx = join_def.column_index(&r_col.name).unwrap();
                    let val_l = l.get(l_idx).unwrap_or(&Value::Null);
                    let val_r = r.get(r_idx).unwrap_or(&Value::Null);
                    if val_l.is_null() || val_r.is_null() || val_l != val_r {
                        return Ok(false);
                    }
                }
            }
            Ok(true)
        }
        Some(JoinConstraint::None) | None => Ok(true),
    }
}

    pub(crate) fn apply_join(
    current_rows: Vec<Row>,
    join_rows: &[Row],
    left_col_count: usize,
    right_col_count: usize,
    join_operator: &JoinOperator,
    ctx: &RowContext,
    base_idx: usize,
    join_def: &TableDef,
) -> H2Result<Vec<Row>> {
    let right_null_vals = vec![Value::Null; right_col_count];
    let left_null_vals = vec![Value::Null; left_col_count];

    match join_operator {
        JoinOperator::Inner(constraint) => {
            let mut new_rows = Vec::new();
            for l in &current_rows {
                for r in join_rows {
                    let mut vals = l.values.clone();
                    vals.extend(r.values.clone());
                    let combined_row = Row::new(vals);
                    if Self::is_join_match(l, r, &combined_row, Some(constraint), ctx, base_idx, join_def)? {
                        new_rows.push(combined_row);
                    }
                }
            }
            Ok(new_rows)
        }
        JoinOperator::LeftOuter(constraint) => {
            let mut new_rows = Vec::new();
            for l in &current_rows {
                let mut matched_any = false;
                for r in join_rows {
                    let mut vals = l.values.clone();
                    vals.extend(r.values.clone());
                    let combined_row = Row::new(vals);
                    if Self::is_join_match(l, r, &combined_row, Some(constraint), ctx, base_idx, join_def)? {
                        new_rows.push(combined_row);
                        matched_any = true;
                    }
                }
                if !matched_any {
                    let mut vals = l.values.clone();
                    vals.extend(right_null_vals.clone());
                    new_rows.push(Row::new(vals));
                }
            }
            Ok(new_rows)
        }
        JoinOperator::RightOuter(constraint) => {
            let mut new_rows = Vec::new();
            let mut right_matched = vec![false; join_rows.len()];

            for l in &current_rows {
                for (j, r) in join_rows.iter().enumerate() {
                    let mut vals = l.values.clone();
                    vals.extend(r.values.clone());
                    let combined_row = Row::new(vals);
                    if Self::is_join_match(l, r, &combined_row, Some(constraint), ctx, base_idx, join_def)? {
                        new_rows.push(combined_row);
                        right_matched[j] = true;
                    }
                }
            }

            for (j, r) in join_rows.iter().enumerate() {
                if !right_matched[j] {
                    let mut vals = left_null_vals.clone();
                    vals.extend(r.values.clone());
                    new_rows.push(Row::new(vals));
                }
            }
            Ok(new_rows)
        }
        JoinOperator::FullOuter(constraint) => {
            let mut new_rows = Vec::new();
            let mut right_matched = vec![false; join_rows.len()];

            for l in &current_rows {
                let mut left_matched_any = false;
                for (j, r) in join_rows.iter().enumerate() {
                    let mut vals = l.values.clone();
                    vals.extend(r.values.clone());
                    let combined_row = Row::new(vals);
                    if Self::is_join_match(l, r, &combined_row, Some(constraint), ctx, base_idx, join_def)? {
                        new_rows.push(combined_row);
                        right_matched[j] = true;
                        left_matched_any = true;
                    }
                }
                if !left_matched_any {
                    let mut vals = l.values.clone();
                    vals.extend(right_null_vals.clone());
                    new_rows.push(Row::new(vals));
                }
            }

            for (j, r) in join_rows.iter().enumerate() {
                if !right_matched[j] {
                    let mut vals = left_null_vals.clone();
                    vals.extend(r.values.clone());
                    new_rows.push(Row::new(vals));
                }
            }
            Ok(new_rows)
        }
        JoinOperator::CrossJoin => {
            let mut new_rows = Vec::new();
            for l in &current_rows {
                for r in join_rows {
                    let mut vals = l.values.clone();
                    vals.extend(r.values.clone());
                    new_rows.push(Row::new(vals));
                }
            }
            Ok(new_rows)
        }
        _ => Err(H2Error::Execution(format!("Unsupported JOIN operator: {:?}", join_operator))),
    }
}

    pub(crate) fn evaluate_table_with_joins(
        &self,
        tx: &Transaction,
        tbl_with_joins: &sqlparser::ast::TableWithJoins,
        current_ctes: &HashMap<String, (TableDef, Vec<Row>)>,
    ) -> H2Result<(RowContext, Vec<Row>)> {
        let (base_def, base_rows, base_alias) = self.resolve_table_factor(tx, &tbl_with_joins.relation, current_ctes)?;
        let mut ctx = RowContext::from_table_def(&base_def, base_alias.as_deref());
        ctx.catalog = Some(Arc::clone(&self.catalog));
        ctx.dialect_mode = Some(self.dialect_mode());
        let mut current_rows = base_rows;

        for join in &tbl_with_joins.joins {
            let (join_def, join_rows, join_alias) = self.resolve_table_factor(tx, &join.relation, current_ctes)?;
            let base_idx = ctx.columns.len();
            let left_col_count = base_idx;
            let right_col_count = join_def.columns.len();
            ctx.append_table(&join_def, join_alias.as_deref(), base_idx);

            current_rows = Self::apply_join(
                current_rows,
                &join_rows,
                left_col_count,
                right_col_count,
                &join.join_operator,
                &ctx,
                base_idx,
                &join_def,
            )?;
        }

        Ok((ctx, current_rows))
    }

    pub(crate) fn matches_index_condition(expr: &Expr, target_col: &str) -> bool {
    match expr {
        Expr::BinaryOp { left, op, right } => {
            if let Expr::Identifier(ident) = left.as_ref() {
                if ident.value.eq_ignore_ascii_case(target_col) {
                    if matches!(op, BinaryOperator::Eq | BinaryOperator::Gt | BinaryOperator::GtEq | BinaryOperator::Lt | BinaryOperator::LtEq) {
                        return evaluate_literal_or_unary(right).is_ok();
                    }
                }
            }
            false
        }
        Expr::Between { expr, low, high, negated } => {
            if !*negated {
                if let Expr::Identifier(ident) = expr.as_ref() {
                    if ident.value.eq_ignore_ascii_case(target_col) {
                        return evaluate_literal_or_unary(low).is_ok() && evaluate_literal_or_unary(high).is_ok();
                    }
                }
            }
            false
        }
        _ => false,
    }
}

    pub(crate) fn extract_equality_predicate(expr: &Expr) -> Option<(String, Value)> {
    match expr {
        Expr::BinaryOp { left, op: BinaryOperator::Eq, right } => {
            let get_ident = |e: &Expr| -> Option<String> {
                match e {
                    Expr::Identifier(ident) => Some(ident.value.clone()),
                    Expr::CompoundIdentifier(parts) if parts.len() == 2 => Some(parts[1].value.clone()),
                    _ => None,
                }
            };
            if let Some(col) = get_ident(left) {
                if let Ok(val) = evaluate_literal_or_unary(right) {
                    return Some((col, val));
                }
            } else if let Some(col) = get_ident(right) {
                if let Ok(val) = evaluate_literal_or_unary(left) {
                    return Some((col, val));
                }
            }
            None
        }
        _ => None,
    }
}

    pub(crate) fn build_index_filter(expr: &Expr, target_col: &str) -> Option<Box<dyn Fn(&Value) -> bool>> {
    match expr {
        Expr::BinaryOp { left, op, right } => {
            if let Expr::Identifier(ident) = left.as_ref() {
                if ident.value.eq_ignore_ascii_case(target_col) {
                    if let Ok(search_val) = evaluate_literal_or_unary(right) {
                        let op_clone = op.clone();
                        return Some(Box::new(move |v: &Value| {
                            match op_clone {
                                BinaryOperator::Eq => v == &search_val,
                                BinaryOperator::Gt => v.partial_cmp(&search_val) == Some(std::cmp::Ordering::Greater),
                                BinaryOperator::GtEq => matches!(v.partial_cmp(&search_val), Some(std::cmp::Ordering::Greater | std::cmp::Ordering::Equal)),
                                BinaryOperator::Lt => v.partial_cmp(&search_val) == Some(std::cmp::Ordering::Less),
                                BinaryOperator::LtEq => matches!(v.partial_cmp(&search_val), Some(std::cmp::Ordering::Less | std::cmp::Ordering::Equal)),
                                _ => false,
                            }
                        }));
                    }
                }
            }
            None
        }
        Expr::Between { expr, low, high, negated } => {
            if !*negated {
                if let Expr::Identifier(ident) = expr.as_ref() {
                    if ident.value.eq_ignore_ascii_case(target_col) {
                        if let (Ok(low_val), Ok(high_val)) = (evaluate_literal_or_unary(low), evaluate_literal_or_unary(high)) {
                            return Some(Box::new(move |v: &Value| {
                                v.partial_cmp(&low_val).map(|c| c != std::cmp::Ordering::Less).unwrap_or(false)
                                    && v.partial_cmp(&high_val).map(|c| c != std::cmp::Ordering::Greater).unwrap_or(false)
                            }));
                        }
                    }
                }
            }
            None
        }
        _ => None,
    }
}

    pub(crate) fn extract_range_predicate(
    expr: &Expr,
    target_col: &str,
) -> Option<(std::ops::Bound<Value>, std::ops::Bound<Value>)> {
    let get_ident = |e: &Expr| -> Option<String> {
        match e {
            Expr::Identifier(ident) => Some(ident.value.clone()),
            Expr::CompoundIdentifier(parts) if parts.len() == 2 => Some(parts[1].value.clone()),
            _ => None,
        }
    };

    match expr {
        Expr::Between { expr, low, high, negated } => {
            if !*negated {
                if let Some(col) = get_ident(expr) {
                    if col.eq_ignore_ascii_case(target_col) {
                        if let (Ok(low_val), Ok(high_val)) = (
                            evaluate_literal_or_unary(low),
                            evaluate_literal_or_unary(high),
                        ) {
                            return Some((std::ops::Bound::Included(low_val), std::ops::Bound::Included(high_val)));
                        }
                    }
                }
            }
            None
        }
        Expr::BinaryOp { left, op, right } => {
            if *op == BinaryOperator::And {
                let left_res = Self::extract_range_predicate(left, target_col);
                let right_res = Self::extract_range_predicate(right, target_col);
                match (left_res, right_res) {
                    (Some((l_start, l_end)), Some((r_start, r_end))) => {
                        let combined_start = match (l_start, r_start) {
                            (std::ops::Bound::Unbounded, b) | (b, std::ops::Bound::Unbounded) => b,
                            (std::ops::Bound::Included(v1), std::ops::Bound::Included(v2)) => {
                                if v1 >= v2 { std::ops::Bound::Included(v1) } else { std::ops::Bound::Included(v2) }
                            }
                            (std::ops::Bound::Excluded(v1), std::ops::Bound::Excluded(v2)) => {
                                if v1 >= v2 { std::ops::Bound::Excluded(v1) } else { std::ops::Bound::Excluded(v2) }
                            }
                            (std::ops::Bound::Included(v1), std::ops::Bound::Excluded(v2)) => {
                                if v1 > v2 { std::ops::Bound::Included(v1) } else { std::ops::Bound::Excluded(v2) }
                            }
                            (std::ops::Bound::Excluded(v1), std::ops::Bound::Included(v2)) => {
                                if v1 >= v2 { std::ops::Bound::Excluded(v1) } else { std::ops::Bound::Included(v2) }
                            }
                        };
                        let combined_end = match (l_end, r_end) {
                            (std::ops::Bound::Unbounded, b) | (b, std::ops::Bound::Unbounded) => b,
                            (std::ops::Bound::Included(v1), std::ops::Bound::Included(v2)) => {
                                if v1 <= v2 { std::ops::Bound::Included(v1) } else { std::ops::Bound::Included(v2) }
                            }
                            (std::ops::Bound::Excluded(v1), std::ops::Bound::Excluded(v2)) => {
                                if v1 <= v2 { std::ops::Bound::Excluded(v1) } else { std::ops::Bound::Excluded(v2) }
                            }
                            (std::ops::Bound::Included(v1), std::ops::Bound::Excluded(v2)) => {
                                if v1 < v2 { std::ops::Bound::Included(v1) } else { std::ops::Bound::Excluded(v2) }
                            }
                            (std::ops::Bound::Excluded(v1), std::ops::Bound::Included(v2)) => {
                                if v1 <= v2 { std::ops::Bound::Excluded(v1) } else { std::ops::Bound::Included(v2) }
                            }
                        };
                        return Some((combined_start, combined_end));
                    }
                    (Some(res), None) => return Some(res),
                    (None, Some(res)) => return Some(res),
                    (None, None) => return None,
                }
            }

            if let Some(col) = get_ident(left) {
                if col.eq_ignore_ascii_case(target_col) {
                    if let Ok(val) = evaluate_literal_or_unary(right) {
                        match op {
                            BinaryOperator::Gt => return Some((std::ops::Bound::Excluded(val), std::ops::Bound::Unbounded)),
                            BinaryOperator::GtEq => return Some((std::ops::Bound::Included(val), std::ops::Bound::Unbounded)),
                            BinaryOperator::Lt => return Some((std::ops::Bound::Unbounded, std::ops::Bound::Excluded(val))),
                            BinaryOperator::LtEq => return Some((std::ops::Bound::Unbounded, std::ops::Bound::Included(val))),
                            _ => {}
                        }
                    }
                }
            } else if let Some(col) = get_ident(right) {
                if col.eq_ignore_ascii_case(target_col) {
                    if let Ok(val) = evaluate_literal_or_unary(left) {
                        match op {
                            BinaryOperator::Gt => return Some((std::ops::Bound::Unbounded, std::ops::Bound::Excluded(val))),
                            BinaryOperator::GtEq => return Some((std::ops::Bound::Unbounded, std::ops::Bound::Included(val))),
                            BinaryOperator::Lt => return Some((std::ops::Bound::Excluded(val), std::ops::Bound::Unbounded)),
                            BinaryOperator::LtEq => return Some((std::ops::Bound::Included(val), std::ops::Bound::Unbounded)),
                            _ => {}
                        }
                    }
                }
            }
            None
        }
        _ => None,
    }
}

    pub(crate) fn convert_value_bounds_to_bytes(
    table_def: &TableDef,
    col_name: &str,
    start: std::ops::Bound<Value>,
    end: std::ops::Bound<Value>,
) -> (std::ops::Bound<Vec<u8>>, std::ops::Bound<Vec<u8>>) {
    let cast_val = |v: Value| -> Value {
        if let Some(c_idx) = table_def.column_index(col_name) {
            v.cast_to(&table_def.columns[c_idx].data_type).unwrap_or(v)
        } else {
            v
        }
    };
    let b_start = match start {
        std::ops::Bound::Included(v) => std::ops::Bound::Included(encode_index_prefix(&[cast_val(v)])),
        std::ops::Bound::Excluded(v) => std::ops::Bound::Excluded(encode_index_key_max(&[cast_val(v)])),
        std::ops::Bound::Unbounded => std::ops::Bound::Unbounded,
    };
    let b_end = match end {
        std::ops::Bound::Included(v) => std::ops::Bound::Included(encode_index_key_max(&[cast_val(v)])),
        std::ops::Bound::Excluded(v) => std::ops::Bound::Excluded(encode_index_prefix(&[cast_val(v)])),
        std::ops::Bound::Unbounded => std::ops::Bound::Unbounded,
    };
    (b_start, b_end)
}

    pub(crate) fn explain_statement(&self, _tx: &Transaction, stmt: Statement) -> H2Result<String> {
        match stmt {
            Statement::Query(query) => {
                let SetExpr::Select(select) = *query.body else {
                    return Ok(format!("EXPLAIN: {:?}", query.body));
                };

                let mut lines = Vec::new();

                // Table / Scan
                if !select.from.is_empty() {
                    let from_table = &select.from[0];
                    let base_table_name = match &from_table.relation {
                        TableFactor::Table { name, .. } => name.to_string(),
                        _ => "unknown".to_string(),
                    };

                    let table_def_opt = self.catalog.get_table(&base_table_name);
                    let mut is_index_scan = false;
                    let mut matched_index_name = String::new();

                    if from_table.joins.is_empty() {
                        if let Some(ref sel) = select.selection {
                            let indexes = self.catalog.get_table_indexes(&base_table_name);
                            for idx in &indexes {
                                if idx.columns.len() == 1 && Self::matches_index_condition(sel, &idx.columns[0]) {
                                    is_index_scan = true;
                                    matched_index_name = idx.name.clone();
                                    break;
                                }
                            }
                        }
                    }

                    let (cost, est_rows) = if let Some(ref t_def) = table_def_opt {
                        crate::stats::estimate_scan_cost(t_def, select.selection.as_ref(), is_index_scan)
                    } else {
                        (1.0, 1)
                    };

                    let scan_type = if is_index_scan {
                        format!("IndexScan: {} on index {} (cost={:.2} rows={})", base_table_name, matched_index_name, cost, est_rows)
                    } else {
                        format!("TableScan: {} (cost={:.2} rows={})", base_table_name, cost, est_rows)
                    };
                    lines.push(scan_type);

                    let current_mode = self.execution_mode();
                    let chosen_exec_mode = if current_mode == "vectorized" || (current_mode == "auto" && est_rows >= 128) {
                        "Vectorized (Apache Arrow)"
                    } else {
                        "Row (Volcano)"
                    };
                    lines.push(format!("ExecutionMode: {}", chosen_exec_mode));

                    for join in &from_table.joins {
                        let join_tbl = match &join.relation {
                            TableFactor::Table { name, .. } => name.to_string(),
                            _ => "unknown".to_string(),
                        };
                        lines.push(format!("NestedLoopJoin: {}", join_tbl));
                    }
                }

                // Filter
                if let Some(selection) = &select.selection {
                    lines.push(format!("Filter: {}", selection));
                }

                // Group By
                match &select.group_by {
                    sqlparser::ast::GroupByExpr::Expressions(exprs, _) if !exprs.is_empty() => {
                        let cols: Vec<String> = exprs.iter().map(|e| e.to_string()).collect();
                        lines.push(format!("Aggregate (Group By: {})", cols.join(", ")));
                    }
                    _ => {}
                }

                // Having
                if let Some(having) = &select.having {
                    lines.push(format!("Having: {}", having));
                }

                // Order By
                if let Some(order_by) = &query.order_by {
                    let items: Vec<String> = order_by.exprs.iter().map(|e| e.to_string()).collect();
                    lines.push(format!("Sort: {}", items.join(", ")));
                }

                // Limit / Offset
                if query.limit.is_some() || query.offset.is_some() {
                    lines.push(format!(
                        "Limit / Offset: limit={:?}, offset={:?}",
                        query.limit.as_ref().map(|l| l.to_string()),
                        query.offset.as_ref().map(|o| o.value.to_string())
                    ));
                }

                // Projection
                let projs: Vec<String> = select.projection.iter().map(|p| p.to_string()).collect();
                lines.push(format!("Projection: {}", projs.join(", ")));

                Ok(lines.join("\n"))
            }
            Statement::Insert(insert) => Ok(format!("Insert into {}", insert.table_name)),
            Statement::Update { table, selection, from, .. } => {
                let table_name = table.relation.to_string();
                let mut lines = vec![format!("Update {}", table_name)];
                let index = if from.is_none() {
                    selection.as_ref().and_then(|sel| Self::extract_equality_predicate(sel)).and_then(|(column, _)| {
                        self.catalog.get_table_indexes(&table_name).into_iter().find(|idx| {
                            idx.columns.len() == 1 && idx.columns[0].eq_ignore_ascii_case(&column)
                        })
                    })
                } else {
                    None
                };
                if let Some(index) = index {
                    lines.push(format!("  -> IndexScan: {} on index {}", table_name, index.name));
                } else {
                    lines.push(format!("  -> TableScan: {}", table_name));
                }
                if let Some(filter) = selection {
                    lines.push(format!("Filter: {}", filter));
                }
                Ok(lines.join("\n"))
            }
            Statement::Delete(_) => Ok("Delete".to_string()),
            _ => Ok(format!("Statement: {:?}", stmt)),
        }
    }

    pub(crate) fn preprocess_subqueries_with_ctes(
        &self,
        tx: &Transaction,
        expr: &Expr,
        ctes: &HashMap<String, (TableDef, Vec<Row>)>,
    ) -> H2Result<Expr> {
        match expr {
            Expr::Subquery(subquery) => {
                let res = self.execute_query_with_ctes(tx, *subquery.clone(), ctes)?;
                let rows = match res {
                    ExecutionResult::Query { rows, .. } => rows,
                    _ => return Err(H2Error::Execution("Subquery must be a query".to_string())),
                };
                let first_val = rows.first().and_then(|r| r.values.first().cloned()).unwrap_or(Value::Null);
                Ok(value_to_sql_expr(first_val))
            }
            Expr::InSubquery { expr: target_expr, subquery, negated } => {
                let processed_target = self.preprocess_subqueries_with_ctes(tx, target_expr, ctes)?;
                let res = self.execute_query_with_ctes(tx, *subquery.clone(), ctes)?;
                let rows = match res {
                    ExecutionResult::Query { rows, .. } => rows,
                    _ => return Err(H2Error::Execution("Subquery must be a query".to_string())),
                };

                let mut list = Vec::with_capacity(rows.len());
                for r in rows {
                    if r.values.len() > 1 {
                        let tuple_items = r.values.into_iter().map(value_to_sql_expr).collect();
                        list.push(Expr::Tuple(tuple_items));
                    } else {
                        let val = r.values.first().cloned().unwrap_or(Value::Null);
                        list.push(value_to_sql_expr(val));
                    }
                }

                Ok(Expr::InList {
                    expr: Box::new(processed_target),
                    list,
                    negated: *negated,
                })
            }
            Expr::Tuple(exprs) => {
                let mut processed = Vec::with_capacity(exprs.len());
                for e in exprs {
                    processed.push(self.preprocess_subqueries_with_ctes(tx, e, ctes)?);
                }
                Ok(Expr::Tuple(processed))
            }
            Expr::Exists { subquery, negated } => {
                let res = self.execute_query_with_ctes(tx, *subquery.clone(), ctes)?;
                let has_rows = match res {
                    ExecutionResult::Query { rows, .. } => !rows.is_empty(),
                    _ => false,
                };
                let matches = if *negated { !has_rows } else { has_rows };
                Ok(Expr::Value(sqlparser::ast::Value::Boolean(matches)))
            }
            Expr::BinaryOp { left, op, right } => {
                let new_left = self.preprocess_subqueries_with_ctes(tx, left, ctes)?;
                let new_right = self.preprocess_subqueries_with_ctes(tx, right, ctes)?;
                Ok(Expr::BinaryOp {
                    left: Box::new(new_left),
                    op: op.clone(),
                    right: Box::new(new_right),
                })
            }
            Expr::UnaryOp { op, expr: inner } => {
                let new_inner = self.preprocess_subqueries_with_ctes(tx, inner, ctes)?;
                Ok(Expr::UnaryOp {
                    op: op.clone(),
                    expr: Box::new(new_inner),
                })
            }
            Expr::Nested(inner) => {
                let new_inner = self.preprocess_subqueries_with_ctes(tx, inner, ctes)?;
                Ok(Expr::Nested(Box::new(new_inner)))
            }
            _ => Ok(expr.clone()),
        }
    }

    pub(crate) fn execute_query(&self, tx: &Transaction, query: Query) -> H2Result<ExecutionResult> {
        let _grant = self.admission.acquire_grant(64 * 1024, std::time::Duration::from_secs(5))?;
        self.execute_query_with_ctes(tx, query, &HashMap::new())
    }

    pub(crate) fn execute_query_with_ctes(
        &self,
        tx: &Transaction,
        query: Query,
        parent_ctes: &HashMap<String, (TableDef, Vec<Row>)>,
    ) -> H2Result<ExecutionResult> {
        h2_types::check_query_timeout()?;
        let mut current_ctes = parent_ctes.clone();
        if let Some(with) = &query.with {
            for cte in &with.cte_tables {
                let cte_name = cte.alias.name.value.clone();

                // 再帰 CTE (WITH RECURSIVE) の評価を試行
                if let SetExpr::SetOperation {
                    op: sqlparser::ast::SetOperator::Union,
                    set_quantifier,
                    left,
                    right,
                } = *cte.query.body.clone()
                {
                    let mut left_query = (*cte.query).clone();
                    left_query.body = left;
                    let anchor_res = self.execute_query_with_ctes(tx, left_query, &current_ctes);

                    if let Ok(ExecutionResult::Query { columns: anchor_cols, rows: anchor_rows }) = anchor_res {
                        let is_distinct = !matches!(set_quantifier, sqlparser::ast::SetQuantifier::All);
                        let final_cols: Vec<String> = if !cte.alias.columns.is_empty() && cte.alias.columns.len() == anchor_cols.len() {
                            cte.alias.columns.iter().map(|c| c.name.value.clone()).collect()
                        } else {
                            anchor_cols
                        };

                        let col_defs: Vec<ColumnDef> = final_cols.iter().map(|c: &String| ColumnDef::new(
                            c.clone(),
                            h2_types::DataType::VarChar(None),
                            true,
                            false,
                        )).collect();
                        let t_def = crate::catalog::TableDef::new(cte_name.clone(), col_defs);

                        let mut all_rows = anchor_rows.clone();
                        let mut working_rows = anchor_rows;
                        let max_iterations = 1000;

                        let mut right_query = (*cte.query).clone();
                        right_query.body = right;

                        let mut iteration = 0;
                        let mut is_recursive = false;

                        while !working_rows.is_empty() && iteration < max_iterations {
                            iteration += 1;
                            let mut iter_ctes = current_ctes.clone();
                            iter_ctes.insert(cte_name.to_lowercase(), (t_def.clone(), working_rows.clone()));

                            match self.execute_query_with_ctes(tx, right_query.clone(), &iter_ctes) {
                                Ok(ExecutionResult::Query { rows: next_rows, .. }) => {
                                    is_recursive = true;
                                    if next_rows.is_empty() {
                                        break;
                                    }
                                    if is_distinct {
                                        let mut new_unique = Vec::new();
                                        for r in next_rows {
                                            if !all_rows.iter().any(|ar| ar.values == r.values)
                                                && !new_unique.iter().any(|nu: &Row| nu.values == r.values)
                                            {
                                                new_unique.push(r);
                                            }
                                        }
                                        if new_unique.is_empty() {
                                            break;
                                        }
                                        all_rows.extend(new_unique.clone());
                                        working_rows = new_unique;
                                    } else {
                                        all_rows.extend(next_rows.clone());
                                        working_rows = next_rows;
                                    }
                                }
                                _ => {
                                    break;
                                }
                            }
                        }

                        if is_recursive {
                            current_ctes.insert(cte_name.to_lowercase(), (t_def, all_rows));
                            continue;
                        }
                    }
                }

                // 通常の非再帰 CTE
                let res = self.execute_query_with_ctes(tx, *cte.query.clone(), &current_ctes)?;
                let (cols, rows) = match res {
                    ExecutionResult::Query { columns, rows } => (columns, rows),
                    _ => return Err(H2Error::Execution("CTE must be a SELECT query".to_string())),
                };
                let final_cols: Vec<String> = if !cte.alias.columns.is_empty() && cte.alias.columns.len() == cols.len() {
                    cte.alias.columns.iter().map(|c| c.name.value.clone()).collect()
                } else {
                    cols
                };
                let col_defs = final_cols.into_iter().map(|c| ColumnDef::new(
                    c,
                    h2_types::DataType::VarChar(None),
                    true,
                    false,
                )).collect();
                let t_def = crate::catalog::TableDef::new(cte_name.clone(), col_defs);
                current_ctes.insert(cte_name.to_lowercase(), (t_def, rows));
            }
        }

        match *query.body.clone() {
            SetExpr::SetOperation { op, set_quantifier, left, right } => {
                let mut left_query = query.clone();
                left_query.body = left;
                left_query.order_by = None;
                left_query.limit = None;
                left_query.offset = None;

                let mut right_query = query.clone();
                right_query.body = right;
                right_query.order_by = None;
                right_query.limit = None;
                right_query.offset = None;

                let (columns, l_rows, r_rows) = match (
                    self.execute_query_with_ctes(tx, left_query, &current_ctes)?,
                    self.execute_query_with_ctes(tx, right_query, &current_ctes)?,
                ) {
                    (
                        ExecutionResult::Query { columns: l_cols, rows: l_rows },
                        ExecutionResult::Query { rows: r_rows, .. },
                    ) => (l_cols, l_rows, r_rows),
                    _ => return Err(H2Error::Execution("Set operation inputs must be queries".to_string())),
                };

                let is_distinct = !matches!(set_quantifier, sqlparser::ast::SetQuantifier::All);

                let mut rows = match op {
                    sqlparser::ast::SetOperator::Union => {
                        let mut combined = l_rows;
                        combined.extend(r_rows);
                        if is_distinct {
                            let mut seen = Vec::new();
                            let mut unique_rows = Vec::new();
                            for r in combined {
                                if !seen.contains(&r.values) {
                                    seen.push(r.values.clone());
                                    unique_rows.push(r);
                                }
                            }
                            unique_rows
                        } else {
                            combined
                        }
                    }
                    sqlparser::ast::SetOperator::Intersect => {
                        if is_distinct {
                            let mut seen = Vec::new();
                            let mut matched_rows = Vec::new();
                            for r in l_rows {
                                if !seen.contains(&r.values) && r_rows.iter().any(|r2| r2.values == r.values) {
                                    seen.push(r.values.clone());
                                    matched_rows.push(r);
                                }
                            }
                            matched_rows
                        } else {
                            let mut r_remaining = r_rows;
                            let mut matched_rows = Vec::new();
                            for r in l_rows {
                                if let Some(pos) = r_remaining.iter().position(|r2| r2.values == r.values) {
                                    r_remaining.remove(pos);
                                    matched_rows.push(r);
                                }
                            }
                            matched_rows
                        }
                    }
                    sqlparser::ast::SetOperator::Except => {
                        if is_distinct {
                            let mut seen = Vec::new();
                            let mut diff_rows = Vec::new();
                            for r in l_rows {
                                if !seen.contains(&r.values) {
                                    seen.push(r.values.clone());
                                    if !r_rows.iter().any(|r2| r2.values == r.values) {
                                        diff_rows.push(r);
                                    }
                                }
                            }
                            diff_rows
                        } else {
                            let mut r_remaining = r_rows;
                            let mut diff_rows = Vec::new();
                            for r in l_rows {
                                if let Some(pos) = r_remaining.iter().position(|r2| r2.values == r.values) {
                                    r_remaining.remove(pos);
                                } else {
                                    diff_rows.push(r);
                                }
                            }
                            diff_rows
                        }
                    }
                };

                if let Some(order_by) = &query.order_by {
                    let res_ctx = RowContext {
                        columns: columns.iter().enumerate().map(|(i, name)| ColumnBinding {
                            table_name: None,
                            table_alias: None,
                            column_name: name.clone(),
                            index: i,
                        }).collect(),
                        catalog: Some(Arc::clone(&self.catalog)),
                        dialect_mode: Some(self.dialect_mode()),
                    };
                    rows.sort_by(|a, b| {
                        for order_expr in &order_by.exprs {
                            let val_a = evaluate_expr_context(&order_expr.expr, &res_ctx, a).unwrap_or(Value::Null);
                            let val_b = evaluate_expr_context(&order_expr.expr, &res_ctx, b).unwrap_or(Value::Null);
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

                let offset_num = if let Some(offset) = &query.offset {
                    match evaluate_literal_or_unary(&offset.value)? {
                        Value::TinyInt(n) => n.max(0) as usize,
                        Value::SmallInt(n) => n.max(0) as usize,
                        Value::Integer(n) => n.max(0) as usize,
                        Value::BigInt(n) => n.max(0) as usize,
                        _ => return Err(H2Error::Execution("OFFSET must be an integer".to_string())),
                    }
                } else {
                    0
                };

                let limit_num = if let Some(limit_expr) = &query.limit {
                    match evaluate_literal_or_unary(limit_expr)? {
                        Value::TinyInt(n) => Some(n.max(0) as usize),
                        Value::SmallInt(n) => Some(n.max(0) as usize),
                        Value::Integer(n) => Some(n.max(0) as usize),
                        Value::BigInt(n) => Some(n.max(0) as usize),
                        _ => return Err(H2Error::Execution("LIMIT must be an integer".to_string())),
                    }
                } else {
                    None
                };

                let final_rows = if let Some(lim) = limit_num {
                    rows.into_iter().skip(offset_num).take(lim).collect()
                } else {
                    rows.into_iter().skip(offset_num).collect()
                };

                return Ok(ExecutionResult::Query {
                    columns,
                    rows: final_rows,
                });
            }
            SetExpr::Select(select) => {
                let select = *select;
                if select.from.is_empty() {
                    let mut ctx = RowContext::new();
                    ctx.catalog = Some(Arc::clone(&self.catalog));
                    ctx.dialect_mode = Some(self.dialect_mode());
                    let dummy_row = Row::new(vec![]);

                    // WHERE 句の評価（サブクエリ展開含む）
                    if let Some(selection) = &select.selection {
                        let proc_sel = self.preprocess_subqueries_with_ctes(tx, selection, &current_ctes)?;
                        match evaluate_expr_context(&proc_sel, &ctx, &dummy_row)? {
                            Value::Boolean(true) => {}
                            _ => {
                                let mut columns = Vec::new();
                                for item in &select.projection {
                                    columns.push(get_select_item_name(item));
                                }
                                return Ok(ExecutionResult::Query { columns, rows: vec![] });
                            }
                        }
                    }

                    let mut result_columns = Vec::new();
                    let mut row_values = Vec::new();
                    for item in &select.projection {
                        result_columns.push(get_select_item_name(item));
                        let val = match item {
                            SelectItem::UnnamedExpr(expr) | SelectItem::ExprWithAlias { expr, .. } => {
                                let proc_expr = self.preprocess_subqueries_with_ctes(tx, expr, &current_ctes)?;
                                evaluate_expr_context(&proc_expr, &ctx, &dummy_row)?
                            }
                            _ => return Err(H2Error::Execution("Wildcard not supported without FROM".to_string())),
                        };
                        row_values.push(val);
                    }

                    return Ok(ExecutionResult::Query {
                        columns: result_columns,
                        rows: vec![Row::new(row_values)],
                    });
                }

                let from_table = &select.from[0];
                let (base_table_def, current_rows, base_table_alias) = match &from_table.relation {
                    TableFactor::Derived { subquery, alias, .. } => {
                        let sub_alias = alias.as_ref().map(|a| a.name.value.clone()).unwrap_or_else(|| "subquery".to_string());
                        let sub_res = self.execute_query_with_ctes(tx, *subquery.clone(), &current_ctes)?;
                        let (cols, rows) = match sub_res {
                            ExecutionResult::Query { columns, rows } => (columns, rows),
                            _ => return Err(H2Error::Execution("Derived table must be a query".to_string())),
                        };
                        let col_defs = cols.into_iter().map(|c| ColumnDef::new(
                            c,
                            h2_types::DataType::VarChar(None),
                            true,
                            false,
                        )).collect();
                        let t_def = crate::catalog::TableDef::new(sub_alias.clone(), col_defs);
                        (t_def, rows, Some(sub_alias))
                    }
                    TableFactor::Table { name, alias, args, .. } => {
                        if let Some(res) = self.resolve_cypher_table_function(tx, name, alias, args)? {
                            res
                        } else {
                            let base_table_name = normalize_object_name(name);
                            let base_table_alias = alias.as_ref().map(|a| a.name.value.clone());

                            let is_dual = base_table_name.eq_ignore_ascii_case("dual")
                                || base_table_name.eq_ignore_ascii_case("sysibm.sysdummy1");

                            if is_dual {
                                let t_def = crate::catalog::TableDef::new(
                                    base_table_name.clone(),
                                    vec![
                                        crate::catalog::ColumnDef::new("dummy", h2_types::DataType::VarChar(Some(1)), false, false),
                                    ],
                                );
                                let rows = vec![Row::new(vec![Value::String("X".to_string())])];
                                (t_def, rows, base_table_alias)
                            } else if let Some(res) = self.resolve_virtual_graph_table(tx, &base_table_name, base_table_alias.clone())? {
                                res
                            } else if let Some((cte_def, cte_rows)) = current_ctes.get(&base_table_name.to_lowercase()) {
                                let mut t_def = cte_def.clone();
                                if let Some(ref a) = base_table_alias {
                                    t_def.name = a.clone();
                                }
                                (t_def, cte_rows.clone(), base_table_alias)
                        } else if let Some(view) = self.catalog.get_view(&base_table_name) {
                            self.resolve_view_query(tx, &view, base_table_alias, &current_ctes)?
                        } else {
                            let is_pg_proc = base_table_name.eq_ignore_ascii_case("pg_proc")
                                || base_table_name.eq_ignore_ascii_case("pg_catalog.pg_proc");
                            let is_info_tables = base_table_name.eq_ignore_ascii_case("information_schema.tables")
                                || base_table_name.eq_ignore_ascii_case("tables");
                            let is_info_columns = base_table_name.eq_ignore_ascii_case("information_schema.columns")
                                || base_table_name.eq_ignore_ascii_case("columns");
                            let is_info_schemata = base_table_name.eq_ignore_ascii_case("information_schema.schemata")
                                || base_table_name.eq_ignore_ascii_case("schemata");

                            let (base_table_def, rows) = if is_pg_proc {
                                let (td, r, _) = self.resolve_pg_proc_table(&base_table_name, None);
                                (td, r)
                            } else if is_info_tables {
                                let t_def = crate::catalog::TableDef::new(
                                    base_table_name.clone(),
                                    vec![
                                        crate::catalog::ColumnDef::new(
                                            "table_name",
                                            h2_types::DataType::VarChar(None),
                                            false,
                                            true,
                                        ),
                                        crate::catalog::ColumnDef::new(
                                            "columns_count",
                                            h2_types::DataType::Integer,
                                            false,
                                            false,
                                        ),
                                        crate::catalog::ColumnDef::new(
                                            "table_rows",
                                            h2_types::DataType::BigInt,
                                            false,
                                            false,
                                        ),
                                    ],
                                );
                                let tables = self.catalog.all_tables();
                                let mut rows = Vec::new();
                                for t in tables {
                                    let row_cnt = t.approx_row_count;
                                    rows.push(Row::new(vec![
                                        Value::String(t.name.clone()),
                                        Value::Integer(t.columns.len() as i32),
                                        Value::BigInt(row_cnt),
                                    ]));
                                }
                                (t_def, rows)
                            } else if is_info_columns {
                                let t_def = crate::catalog::TableDef::new(
                                    base_table_name.clone(),
                                    vec![
                                        crate::catalog::ColumnDef::new(
                                            "table_name",
                                            h2_types::DataType::VarChar(None),
                                            false,
                                            false,
                                        ),
                                        crate::catalog::ColumnDef::new(
                                            "column_name",
                                            h2_types::DataType::VarChar(None),
                                            false,
                                            false,
                                        ),
                                        crate::catalog::ColumnDef::new(
                                            "data_type",
                                            h2_types::DataType::VarChar(None),
                                            false,
                                            false,
                                        ),
                                        crate::catalog::ColumnDef::new(
                                            "is_nullable",
                                            h2_types::DataType::Boolean,
                                            false,
                                            false,
                                        ),
                                    ],
                                );
                                let tables = self.catalog.all_tables();
                                let mut rows = Vec::new();
                                for t in tables {
                                    for c in &t.columns {
                                        rows.push(Row::new(vec![
                                            Value::String(t.name.clone()),
                                            Value::String(c.name.clone()),
                                            Value::String(c.data_type.to_string()),
                                            Value::Boolean(c.is_nullable),
                                        ]));
                                    }
                                }
                                (t_def, rows)
                            } else if is_info_schemata {
                                let t_def = crate::catalog::TableDef::new(
                                    base_table_name.clone(),
                                    vec![
                                        crate::catalog::ColumnDef::new(
                                            "schema_name",
                                            h2_types::DataType::VarChar(None),
                                            false,
                                            true,
                                        ),
                                    ],
                                );
                                let mut schemas = self.catalog.get_schemas();
                                schemas.sort();
                                let mut rows = Vec::new();
                                for s in schemas {
                                    rows.push(Row::new(vec![Value::String(s)]));
                                }
                                (t_def, rows)
                            } else {
                                let table_def = self.catalog.get_table(&base_table_name).ok_or_else(|| {
                                    H2Error::Catalog(format!("Table '{}' not found", base_table_name))
                                })?;

                                if table_def.is_queue {
                                    if let Some(selection) = &select.selection {
                                        validate_queue_where_clause(selection)?;
                                    }
                                }


                                let map_name = table_def.map_name();

                                let mut ctx = RowContext::from_table_def(&table_def, base_table_alias.as_deref());
                                ctx.catalog = Some(Arc::clone(&self.catalog));
                                ctx.dialect_mode = Some(self.dialect_mode());

                                let is_simple_agg = from_table.joins.is_empty()
                                    && match &select.group_by {
                                        GroupByExpr::Expressions(exprs, _) => exprs.is_empty(),
                                        _ => true,
                                    }
                                    && select.having.is_none();
                                let pushdown_ops = if is_simple_agg {
                                    Self::parse_aggregate_ops(&select.projection, &ctx)
                                } else {
                                    None
                                };

                                // 0. 全表走査のプッシュダウン集約（Full Scan Aggregate）
                                if let Some((ref col_names, ref ops)) = pushdown_ops {
                                    if select.selection.is_none() {
                                        let mut sums = vec![0.0f64; ops.len()];
                                        let mut counts = vec![0i64; ops.len()];
                                        let mut mins = vec![f64::MAX; ops.len()];
                                        let mut maxs = vec![f64::MIN; ops.len()];
                                        let mut min_ints = vec![i64::MAX; ops.len()];
                                        let mut max_ints = vec![i64::MIN; ops.len()];

                                        tx.for_each_visible(&map_name, |_k, val_bytes| {
                                            for (i, op) in ops.iter().enumerate() {
                                                match op {
                                                    crate::vectorized::VectorAggregateOp::CountStar => {
                                                        counts[i] += 1;
                                                    }
                                                    crate::vectorized::VectorAggregateOp::Count(col) => {
                                                        if crate::row::extract_numeric_column(val_bytes, *col).is_some() {
                                                            counts[i] += 1;
                                                        }
                                                    }
                                                    crate::vectorized::VectorAggregateOp::Sum(col) => {
                                                        if let Some((_vi, vf)) = crate::row::extract_numeric_column(val_bytes, *col) {
                                                            sums[i] += vf;
                                                            counts[i] += 1;
                                                        }
                                                    }
                                                    crate::vectorized::VectorAggregateOp::Avg(col) => {
                                                        if let Some((_vi, vf)) = crate::row::extract_numeric_column(val_bytes, *col) {
                                                            sums[i] += vf;
                                                            counts[i] += 1;
                                                        }
                                                    }
                                                    crate::vectorized::VectorAggregateOp::Min(col) => {
                                                        if let Some((vi, vf)) = crate::row::extract_numeric_column(val_bytes, *col) {
                                                            if vf < mins[i] { mins[i] = vf; }
                                                            if vi < min_ints[i] { min_ints[i] = vi; }
                                                            counts[i] += 1;
                                                        }
                                                    }
                                                    crate::vectorized::VectorAggregateOp::Max(col) => {
                                                        if let Some((vi, vf)) = crate::row::extract_numeric_column(val_bytes, *col) {
                                                            if vf > maxs[i] { maxs[i] = vf; }
                                                            if vi > max_ints[i] { max_ints[i] = vi; }
                                                            counts[i] += 1;
                                                        }
                                                    }
                                                }
                                            }
                                        })?;

                                        let res = Self::build_pushdown_aggregate_result(ops, col_names, &counts, &sums, &mins, &maxs, &min_ints, &max_ints, &table_def);
                                        return Ok(res);
                                    }
                                }

                                // IndexScan の最適化 (等値 Point Lookup & Range Scan 対応)
                                let mut index_scanned: Option<Vec<Row>> = None;
                                if from_table.joins.is_empty() {
                                    if let Some(ref sel) = select.selection {
                                        let indexes = self.catalog.get_table_indexes(&base_table_name);

                                        // 1. 等値検索（Point Lookup）の高速B+Tree探索
                                        if let Some((col_name, val)) = Self::extract_equality_predicate(sel) {
                                            for target_idx in &indexes {
                                                if target_idx.columns.len() == 1 && target_idx.columns[0].eq_ignore_ascii_case(&col_name) {
                                                    let idx_map_name = format!("idx_{}_{}", table_def.name.to_lowercase(), target_idx.name.to_lowercase());
                                                    let casted_val = if let Some(c_idx) = table_def.column_index(&col_name) {
                                                        val.cast_to(&table_def.columns[c_idx].data_type).unwrap_or(val.clone())
                                                    } else {
                                                        val.clone()
                                                    };

                                                    let prefix = encode_index_prefix(std::slice::from_ref(&casted_val));

                                                    let matched_entries = tx.scan_prefix_visible(&idx_map_name, &prefix)?;
                                                    let mut fetched = Vec::with_capacity(matched_entries.len());
                                                    for (k, _) in matched_entries {
                                                        if let Some((_v, r_id)) = decode_index_key(&k) {
                                                            if let Some(val_bytes) = tx.get(&map_name, &r_id.to_le_bytes())? {
                                                                let mut row = Row::from_bytes(&val_bytes)?;
                                                                table_def.align_row(&mut row);
                                                                fetched.push(row);
                                                            }
                                                        }
                                                    }

                                                    // Point Select ファストパス: 単一ユニーク行かつ単純射影
                                                    if target_idx.is_unique && from_table.joins.is_empty() && pushdown_ops.is_none() {
                                                        let is_agg = select.projection.iter().any(|item| match item {
                                                             SelectItem::UnnamedExpr(expr) | SelectItem::ExprWithAlias { expr, .. } => has_aggregate_func(expr),
                                                             _ => false,
                                                        });
                                                        let has_group = match &select.group_by {
                                                             GroupByExpr::Expressions(exprs, _) => !exprs.is_empty(),
                                                             _ => false,
                                                        };
                                                        let is_simple_projection = !select.projection.is_empty() && select.projection.iter().all(|item| match item {
                                                            SelectItem::UnnamedExpr(Expr::Identifier(ident)) | SelectItem::ExprWithAlias { expr: Expr::Identifier(ident), .. } => {
                                                                table_def.column_index(&ident.value.to_lowercase()).is_some()
                                                            }
                                                            SelectItem::Wildcard(_) => true,
                                                            _ => false,
                                                        });
                                                        if !is_agg && !has_group && is_simple_projection && query.order_by.is_none() && query.limit.is_none() {
                                                            let mut out_cols = Vec::new();
                                                            let mut out_rows = Vec::new();
                                                            for row in &fetched {
                                                                let mut row_vals = Vec::new();
                                                                for item in &select.projection {
                                                                    match item {
                                                                        SelectItem::UnnamedExpr(Expr::Identifier(ident)) | SelectItem::ExprWithAlias { expr: Expr::Identifier(ident), .. } => {
                                                                            let c_name = ident.value.to_lowercase();
                                                                            if let Some(c_idx) = table_def.column_index(&c_name) {
                                                                                row_vals.push(row.values.get(c_idx).cloned().unwrap_or(Value::Null));
                                                                            }
                                                                        }
                                                                        SelectItem::Wildcard(_) => {
                                                                            row_vals.extend(row.values.clone());
                                                                        }
                                                                        _ => {}
                                                                    }
                                                                }
                                                                out_rows.push(Row::new(row_vals));
                                                            }
                                                            for item in &select.projection {
                                                                out_cols.push(get_select_item_name(item));
                                                            }
                                                            return Ok(ExecutionResult::Query { columns: out_cols, rows: out_rows });
                                                        }
                                                    }

                                                    index_scanned = Some(fetched);
                                                    break;
                                                }
                                            }
                                        }

                                        // 2. Range Scan 等のインデックス検索 (B+Tree 高速範囲検索)
                                        if index_scanned.is_none() {
                                            for target_idx in &indexes {
                                                if target_idx.columns.len() == 1 {
                                                    let col_name = &target_idx.columns[0];
                                                    if let Some((start_bound, end_bound)) = Self::extract_range_predicate(sel, col_name) {
                                                        let idx_map_name = format!("idx_{}_{}", table_def.name.to_lowercase(), target_idx.name.to_lowercase());
                                                        let (b_start, b_end) = Self::convert_value_bounds_to_bytes(&table_def, col_name, start_bound, end_bound);
                                                        let matched = tx.scan_range_visible(
                                                            &idx_map_name,
                                                            match &b_start {
                                                                std::ops::Bound::Included(b) => std::ops::Bound::Included(b.as_slice()),
                                                                std::ops::Bound::Excluded(b) => std::ops::Bound::Excluded(b.as_slice()),
                                                                std::ops::Bound::Unbounded => std::ops::Bound::Unbounded,
                                                            },
                                                            match &b_end {
                                                                std::ops::Bound::Included(b) => std::ops::Bound::Included(b.as_slice()),
                                                                std::ops::Bound::Excluded(b) => std::ops::Bound::Excluded(b.as_slice()),
                                                                std::ops::Bound::Unbounded => std::ops::Bound::Unbounded,
                                                            },
                                                        )?;

                                                        // Range Scan プッシュダウン集約
                                                        if let Some((ref col_names, ref ops)) = pushdown_ops {
                                                            let mut sums = vec![0.0f64; ops.len()];
                                                            let mut counts = vec![0i64; ops.len()];
                                                            let mut mins = vec![f64::MAX; ops.len()];
                                                            let mut maxs = vec![f64::MIN; ops.len()];
                                                            let mut min_ints = vec![i64::MAX; ops.len()];
                                                            let mut max_ints = vec![i64::MIN; ops.len()];

                                                            for (k, _) in &matched {
                                                                if k.len() >= 8 {
                                                                    let r_id = u64::from_be_bytes(k[k.len() - 8..].try_into().unwrap());
                                                                    if let Some(val_bytes) = tx.get(&map_name, &r_id.to_le_bytes())? {
                                                                        for (i, op) in ops.iter().enumerate() {
                                                                            match op {
                                                                                crate::vectorized::VectorAggregateOp::CountStar => {
                                                                                    counts[i] += 1;
                                                                                }
                                                                                crate::vectorized::VectorAggregateOp::Count(col) => {
                                                                                    if crate::row::extract_numeric_column(&val_bytes, *col).is_some() {
                                                                                        counts[i] += 1;
                                                                                    }
                                                                                }
                                                                                crate::vectorized::VectorAggregateOp::Sum(col) => {
                                                                                    if let Some((_vi, vf)) = crate::row::extract_numeric_column(&val_bytes, *col) {
                                                                                        sums[i] += vf;
                                                                                        counts[i] += 1;
                                                                                    }
                                                                                }
                                                                                crate::vectorized::VectorAggregateOp::Avg(col) => {
                                                                                    if let Some((_vi, vf)) = crate::row::extract_numeric_column(&val_bytes, *col) {
                                                                                        sums[i] += vf;
                                                                                        counts[i] += 1;
                                                                                    }
                                                                                }
                                                                                crate::vectorized::VectorAggregateOp::Min(col) => {
                                                                                    if let Some((vi, vf)) = crate::row::extract_numeric_column(&val_bytes, *col) {
                                                                                        if vf < mins[i] { mins[i] = vf; }
                                                                                        if vi < min_ints[i] { min_ints[i] = vi; }
                                                                                        counts[i] += 1;
                                                                                    }
                                                                                }
                                                                                crate::vectorized::VectorAggregateOp::Max(col) => {
                                                                                    if let Some((vi, vf)) = crate::row::extract_numeric_column(&val_bytes, *col) {
                                                                                        if vf > maxs[i] { maxs[i] = vf; }
                                                                                        if vi > max_ints[i] { max_ints[i] = vi; }
                                                                                        counts[i] += 1;
                                                                                    }
                                                                                }
                                                                            }
                                                                        }
                                                                    }
                                                                }
                                                            }
                                                            let res = Self::build_pushdown_aggregate_result(ops, col_names, &counts, &sums, &mins, &maxs, &min_ints, &max_ints, &table_def);
                                                            return Ok(res);
                                                        }

                                                        let mut fetched = Vec::with_capacity(matched.len());
                                                        for (k, _) in matched {
                                                            if let Some((_v, r_id)) = decode_index_key(&k) {
                                                                if let Some(val_bytes) = tx.get(&map_name, &r_id.to_le_bytes())? {
                                                                    let mut row = Row::from_bytes(&val_bytes)?;
                                                                    table_def.align_row(&mut row);
                                                                    fetched.push(row);
                                                                }
                                                            }
                                                        }
                                                        index_scanned = Some(fetched);
                                                        break;
                                                    }
                                                }

                                                if let Some(filter_fn) = Self::build_index_filter(sel, &target_idx.columns[0]) {
                                                    let idx_map_name = format!("idx_{}_{}", table_def.name.to_lowercase(), target_idx.name.to_lowercase());
                                                    let idx_entries = tx.scan_visible(&idx_map_name)?;
                                                    let mut matched_row_ids = Vec::new();
                                                    for (k, _) in idx_entries {
                                                        if let Some((v, r_id)) = decode_index_key(&k) {
                                                            if filter_fn(&v) {
                                                                matched_row_ids.push(r_id);
                                                            }
                                                        }
                                                    }
                                                    let mut fetched = Vec::new();
                                                    for r_id in matched_row_ids {
                                                        if let Some(val_bytes) = tx.get(&map_name, &r_id.to_le_bytes())? {
                                                            let mut row = Row::from_bytes(&val_bytes)?;
                                                            table_def.align_row(&mut row);
                                                            fetched.push(row);
                                                        }
                                                    }
                                                    index_scanned = Some(fetched);
                                                    break;
                                                }
                                            }
                                        }
                                    }
                                }

                                let rows = if let Some(r) = index_scanned {
                                    r
                                } else {
                                    let entries = tx.scan_visible(&map_name)?;
                                    let mut current_rows = Vec::with_capacity(entries.len());
                                    for (_k, val_bytes) in entries {
                                        let mut row = Row::from_bytes(&val_bytes)?;
                                        table_def.align_row(&mut row);
                                        current_rows.push(row);
                                    }
                                    current_rows
                                };
                                (table_def, rows)
                            };
                            (base_table_def, rows, base_table_alias)
                        }
                    }
                }
                    _ => return Err(H2Error::Execution("Complex table factors not supported".to_string())),
                };

                let mut current_rows = current_rows;
                let mut ctx = RowContext::from_table_def(&base_table_def, base_table_alias.as_deref());
                ctx.catalog = Some(Arc::clone(&self.catalog));
                ctx.dialect_mode = Some(self.dialect_mode());

                // JOIN の処理
                for join in &from_table.joins {
                    let (join_table_def, join_rows, join_table_alias) = self.resolve_table_factor(tx, &join.relation, &current_ctes)?;
                    let base_idx = ctx.columns.len();
                    let left_col_count = base_idx;
                    let right_col_count = join_table_def.columns.len();
                    ctx.append_table(&join_table_def, join_table_alias.as_deref(), base_idx);

                    current_rows = Self::apply_join(
                        current_rows,
                        &join_rows,
                        left_col_count,
                        right_col_count,
                        &join.join_operator,
                        &ctx,
                        base_idx,
                        &join_table_def,
                    )?;
                }

                // WHERE 句のフィルタリング（サブクエリ展開含む）
                let filtered_rows = if let Some(selection) = &select.selection {
                    let proc_sel = self.preprocess_subqueries_with_ctes(tx, selection, &current_ctes)?;
                    let mut matched = Vec::new();
                    for row in current_rows {
                        if let Value::Boolean(true) = evaluate_expr_context(&proc_sel, &ctx, &row)? {
                            matched.push(row);
                        }
                    }
                    matched
                } else {
                    current_rows
                };

                // GROUP BY / 集約関数の判定
                let group_by_exprs = match &select.group_by {
                    GroupByExpr::Expressions(exprs, _) => exprs.clone(),
                    _ => Vec::new(),
                };

                let has_agg = select.projection.iter().any(|item| match item {
                    SelectItem::UnnamedExpr(expr) | SelectItem::ExprWithAlias { expr, .. } => has_aggregate_func(expr),
                    _ => false,
                });

                let is_aggregate = !group_by_exprs.is_empty() || has_agg || select.having.is_some();

                let (result_columns, projected_rows) = if is_aggregate {
                    let (result_columns, mut rows) = if group_by_exprs.is_empty() && select.having.is_none() {
                        if let Some((vec_cols, vec_rows)) = self.try_execute_vectorized_aggregate(
                            &select.projection,
                            &filtered_rows,
                            &base_table_def,
                            &ctx,
                        ) {
                            (vec_cols, vec_rows)
                        } else {
                            let groups = vec![(Vec::<Value>::new(), filtered_rows)];

                            let mut result_columns = Vec::new();
                            for item in &select.projection {
                                result_columns.push(get_select_item_name(item));
                            }

                            let mut rows = Vec::new();
                            for (_key, group_rows) in groups {
                                let mut row_vals = Vec::new();
                                for item in &select.projection {
                                    let val = match item {
                                        SelectItem::UnnamedExpr(expr) | SelectItem::ExprWithAlias { expr, .. } => {
                                            let proc_expr = self.preprocess_subqueries_with_ctes(tx, expr, &current_ctes)?;
                                            evaluate_aggregate_expr(&proc_expr, &ctx, &group_rows)?
                                        }
                                        SelectItem::Wildcard(_) => {
                                            return Err(H2Error::Execution("Wildcard in aggregate query not supported".to_string()));
                                        }
                                        _ => return Err(H2Error::Execution("Unsupported select item".to_string())),
                                    };
                                    row_vals.push(val);
                                }
                                rows.push(Row::new(row_vals));
                            }
                            (result_columns, rows)
                        }
                    } else {
                        let mut groups: Vec<(Vec<Value>, Vec<Row>)> = Vec::new();
                        for row in filtered_rows {
                            let mut key = Vec::with_capacity(group_by_exprs.len());
                            for expr in &group_by_exprs {
                                key.push(evaluate_expr_context(expr, &ctx, &row)?);
                            }
                            if let Some(pos) = groups.iter().position(|(k, _)| k == &key) {
                                groups[pos].1.push(row);
                            } else {
                                groups.push((key, vec![row]));
                            }
                        }

                        if groups.is_empty() && group_by_exprs.is_empty() {
                            groups.push((Vec::new(), Vec::new()));
                        }

                        let mut result_columns = Vec::new();
                        for item in &select.projection {
                            result_columns.push(get_select_item_name(item));
                        }

                        let mut rows = Vec::new();
                        for (_key, group_rows) in groups {
                            let mut row_vals = Vec::new();
                            for item in &select.projection {
                                let val = match item {
                                    SelectItem::UnnamedExpr(expr) | SelectItem::ExprWithAlias { expr, .. } => {
                                        let proc_expr = self.preprocess_subqueries_with_ctes(tx, expr, &current_ctes)?;
                                        evaluate_aggregate_expr(&proc_expr, &ctx, &group_rows)?
                                    }
                                    SelectItem::Wildcard(_) => {
                                        return Err(H2Error::Execution("Wildcard in aggregate query not supported".to_string()));
                                    }
                                    _ => return Err(H2Error::Execution("Unsupported select item".to_string())),
                                };
                                row_vals.push(val);
                            }
                            let agg_row = Row::new(row_vals);

                            // HAVING 句の評価
                            let mut matches_having = true;
                            if let Some(having) = &select.having {
                                let proc_having = self.preprocess_subqueries_with_ctes(tx, having, &current_ctes)?;
                                let having_val = evaluate_aggregate_expr(&proc_having, &ctx, &group_rows)?;
                                if let Value::Boolean(b) = having_val {
                                    matches_having = b;
                                } else {
                                    matches_having = false;
                                }
                            }

                            if matches_having {
                                rows.push(agg_row);
                            }
                        }
                        (result_columns, rows)
                    };

                    // 集約クエリの ORDER BY
                    if let Some(order_by) = &query.order_by {
                        let res_ctx = RowContext {
                            columns: result_columns.iter().enumerate().map(|(i, name)| ColumnBinding {
                                table_name: None,
                                table_alias: None,
                                column_name: name.clone(),
                                index: i,
                            }).collect(),
                            catalog: Some(Arc::clone(&self.catalog)),
                            dialect_mode: Some(self.dialect_mode()),
                        };

                        let work_mem = self.memory_config.work_mem();
                        let mut sorter = crate::memory::ExternalSorter::new(work_mem, |a: &Row, b: &Row| {
                            for order_expr in &order_by.exprs {
                                let val_a = evaluate_expr_context(&order_expr.expr, &res_ctx, a).unwrap_or(Value::Null);
                                let val_b = evaluate_expr_context(&order_expr.expr, &res_ctx, b).unwrap_or(Value::Null);
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
                        for r in rows {
                            sorter.add_row(r)?;
                        }
                        rows = sorter.finish()?;
                    }

                    (result_columns, rows)
                } else {
                    let is_wildcard = select.projection.iter().any(|p| matches!(p, SelectItem::Wildcard(_)));
                    let mut result_columns = Vec::new();

                    if is_wildcard {
                        for col in &ctx.columns {
                            result_columns.push(col.column_name.clone());
                        }
                    } else {
                        for item in &select.projection {
                            result_columns.push(get_select_item_name(item));
                        }
                    }

                    // ソート用に行を準備
                    let mut sort_ctx = ctx.clone();
                    let base_col_len = ctx.columns.len();
                    if !is_wildcard {
                        for (i, name) in result_columns.iter().enumerate() {
                            sort_ctx.columns.push(ColumnBinding {
                                table_name: None,
                                table_alias: None,
                                column_name: name.clone(),
                                index: base_col_len + i,
                            });
                        }
                    }

                    let mut window_funcs = Vec::new();
                    for item in &select.projection {
                        match item {
                            SelectItem::UnnamedExpr(expr) | SelectItem::ExprWithAlias { expr, .. } => {
                                collect_window_functions(expr, &mut window_funcs);
                            }
                            _ => {}
                        }
                    }

                    let computed_windows = if !window_funcs.is_empty() {
                        compute_window_functions(&window_funcs, &filtered_rows, &ctx)?
                    } else {
                        HashMap::new()
                    };

                    let mut extended_rows = Vec::with_capacity(filtered_rows.len());
                    for (row_idx, row) in filtered_rows.into_iter().enumerate() {
                        if is_wildcard {
                            extended_rows.push(row);
                        } else {
                            let mut proj_vals = Vec::with_capacity(select.projection.len());
                            for item in &select.projection {
                                let val = match item {
                                    SelectItem::UnnamedExpr(expr) | SelectItem::ExprWithAlias { expr, .. } => {
                                        let mut proc_expr = self.preprocess_subqueries_with_ctes(tx, expr, &current_ctes)?;
                                        if !computed_windows.is_empty() {
                                            for (func_str, vals) in &computed_windows {
                                                proc_expr = replace_window_function(&proc_expr, func_str, &vals[row_idx]);
                                            }
                                        }
                                        evaluate_expr_context(&proc_expr, &ctx, &row)?
                                    }
                                    _ => return Err(H2Error::Execution("Unsupported select item".to_string())),
                                };
                                proj_vals.push(val);
                            }
                            let mut full_vals = row.values;
                            full_vals.extend(proj_vals);
                            extended_rows.push(Row::new(full_vals));
                        }
                    }

                    // ORDER BY
                    if let Some(order_by) = &query.order_by {
                        let work_mem = self.memory_config.work_mem();
                        let mut sorter = crate::memory::ExternalSorter::new(work_mem, |a: &Row, b: &Row| {
                            for order_expr in &order_by.exprs {
                                let val_a = evaluate_expr_context(&order_expr.expr, &sort_ctx, a).unwrap_or(Value::Null);
                                let val_b = evaluate_expr_context(&order_expr.expr, &sort_ctx, b).unwrap_or(Value::Null);
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
                        for r in extended_rows {
                            sorter.add_row(r)?;
                        }
                        extended_rows = sorter.finish()?;
                    }

                    let mut rows = Vec::with_capacity(extended_rows.len());
                    if is_wildcard {
                        rows = extended_rows;
                    } else {
                        for ext_row in extended_rows {
                            let proj_vals = ext_row.values[base_col_len..].to_vec();
                            rows.push(Row::new(proj_vals));
                        }
                    }

                    (result_columns, rows)
                };

                // DISTINCT
                let mut final_rows = projected_rows;
                if select.distinct.is_some() {
                    let mut seen = Vec::new();
                    let mut unique_rows = Vec::new();
                    for row in final_rows {
                        if !seen.contains(&row.values) {
                            seen.push(row.values.clone());
                            unique_rows.push(row);
                        }
                    }
                    final_rows = unique_rows;
                }

                // LIMIT / OFFSET スライス
                let offset_num = if let Some(offset) = &query.offset {
                    match evaluate_literal_or_unary(&offset.value)? {
                        Value::TinyInt(n) => n.max(0) as usize,
                        Value::SmallInt(n) => n.max(0) as usize,
                        Value::Integer(n) => n.max(0) as usize,
                        Value::BigInt(n) => n.max(0) as usize,
                        _ => return Err(H2Error::Execution("OFFSET must be an integer".to_string())),
                    }
                } else {
                    0
                };

                let limit_num = if let Some(limit_expr) = &query.limit {
                    match evaluate_literal_or_unary(limit_expr)? {
                        Value::TinyInt(n) => Some(n.max(0) as usize),
                        Value::SmallInt(n) => Some(n.max(0) as usize),
                        Value::Integer(n) => Some(n.max(0) as usize),
                        Value::BigInt(n) => Some(n.max(0) as usize),
                        _ => return Err(H2Error::Execution("LIMIT must be an integer".to_string())),
                    }
                } else {
                    None
                };

                let final_rows: Vec<Row> = if let Some(lim) = limit_num {
                    final_rows.into_iter().skip(offset_num).take(lim).collect()
                } else {
                    final_rows.into_iter().skip(offset_num).collect()
                };

                let max_rows = self.memory_config.max_materialized_rows();
                if max_rows > 0 && final_rows.len() > max_rows {
                    return Err(H2Error::Execution(format!(
                        "Query exceeded maximum materialized row limit ({}); consider adding LIMIT or pagination",
                        max_rows
                    )));
                }

                Ok(ExecutionResult::Query {
                    columns: result_columns,
                    rows: final_rows,
                })
            }
            _ => Err(H2Error::Execution("Only SELECT queries or UNION are supported".to_string())),
        }
    }

    // ================= DCL (ユーザー・権限管理) 実装 =================

}

pub(crate) fn has_aggregate_func(expr: &Expr) -> bool {
    match expr {
        Expr::Function(func) => {
            if func.over.is_some() {
                return false;
            }
            let name = func.name.to_string().to_uppercase();
            matches!(name.as_str(), "COUNT" | "SUM" | "AVG" | "MIN" | "MAX")
        }
        Expr::BinaryOp { left, right, .. } => has_aggregate_func(left) || has_aggregate_func(right),
        Expr::UnaryOp { expr, .. } | Expr::Nested(expr) => has_aggregate_func(expr),
        _ => false,
    }
}

pub(crate) fn get_select_item_name(item: &SelectItem) -> String {
    match item {
        SelectItem::UnnamedExpr(expr) => match expr {
            Expr::Identifier(ident) => ident.value.clone(),
            Expr::CompoundIdentifier(idents) => idents.iter().map(|i| i.value.as_str()).collect::<Vec<_>>().join("."),
            Expr::Function(func) => func.to_string(),
            _ => expr.to_string(),
        },
        SelectItem::ExprWithAlias { alias, .. } => alias.value.clone(),
        SelectItem::Wildcard(_) => "*".to_string(),
        _ => "col".to_string(),
    }
}

pub(crate) fn evaluate_aggregate_expr(expr: &Expr, ctx: &RowContext, rows: &[Row]) -> H2Result<Value> {
    match expr {
        Expr::Function(func) => {
            let func_name = func.name.to_string().to_uppercase();
            let args = match &func.args {
                sqlparser::ast::FunctionArguments::List(arg_list) => &arg_list.args,
                sqlparser::ast::FunctionArguments::None => &Vec::new(),
                _ => return Err(H2Error::Execution("Invalid aggregate function arguments".to_string())),
            };

            match func_name.as_str() {
                "COUNT" => {
                    if args.is_empty() {
                        return Ok(Value::BigInt(rows.len() as i64));
                    }
                    match &args[0] {
                        FunctionArg::Unnamed(FunctionArgExpr::Wildcard) => {
                            Ok(Value::BigInt(rows.len() as i64))
                        }
                        FunctionArg::Unnamed(FunctionArgExpr::Expr(arg_expr)) => {
                            let mut count = 0i64;
                            for row in rows {
                                let val = evaluate_expr_context(arg_expr, ctx, row)?;
                                if !val.is_null() {
                                    count += 1;
                                }
                            }
                            Ok(Value::BigInt(count))
                        }
                        _ => Ok(Value::BigInt(rows.len() as i64)),
                    }
                }
                "SUM" => {
                    if args.is_empty() {
                        return Err(H2Error::Execution("SUM requires 1 argument".to_string()));
                    }
                    let arg_expr = match &args[0] {
                        FunctionArg::Unnamed(FunctionArgExpr::Expr(e)) => e,
                        _ => return Err(H2Error::Execution("Invalid SUM argument".to_string())),
                    };

                    let mut sum_val: Option<Value> = None;
                    for row in rows {
                        let val = evaluate_expr_context(arg_expr, ctx, row)?;
                        if !val.is_null() {
                            sum_val = match sum_val {
                                None => Some(val),
                                Some(curr) => Some(add_values(&curr, &val)?),
                            };
                        }
                    }
                    Ok(sum_val.unwrap_or(Value::Null))
                }
                "AVG" => {
                    if args.is_empty() {
                        return Err(H2Error::Execution("AVG requires 1 argument".to_string()));
                    }
                    let arg_expr = match &args[0] {
                        FunctionArg::Unnamed(FunctionArgExpr::Expr(e)) => e,
                        _ => return Err(H2Error::Execution("Invalid AVG argument".to_string())),
                    };

                    let mut sum_f64 = 0.0;
                    let mut count = 0i64;
                    for row in rows {
                        let val = evaluate_expr_context(arg_expr, ctx, row)?;
                        if let Some(f) = val.to_f64() {
                            sum_f64 += f;
                            count += 1;
                        }
                    }
                    if count == 0 {
                        Ok(Value::Null)
                    } else {
                        Ok(Value::Double(sum_f64 / count as f64))
                    }
                }
                "MIN" => {
                    if args.is_empty() {
                        return Err(H2Error::Execution("MIN requires 1 argument".to_string()));
                    }
                    let arg_expr = match &args[0] {
                        FunctionArg::Unnamed(FunctionArgExpr::Expr(e)) => e,
                        _ => return Err(H2Error::Execution("Invalid MIN argument".to_string())),
                    };

                    let mut min_val: Option<Value> = None;
                    for row in rows {
                        let val = evaluate_expr_context(arg_expr, ctx, row)?;
                        if !val.is_null() {
                            min_val = match min_val {
                                None => Some(val),
                                Some(curr) => {
                                    if val < curr {
                                        Some(val)
                                    } else {
                                        Some(curr)
                                    }
                                }
                            };
                        }
                    }
                    Ok(min_val.unwrap_or(Value::Null))
                }
                "MAX" => {
                    if args.is_empty() {
                        return Err(H2Error::Execution("MAX requires 1 argument".to_string()));
                    }
                    let arg_expr = match &args[0] {
                        FunctionArg::Unnamed(FunctionArgExpr::Expr(e)) => e,
                        _ => return Err(H2Error::Execution("Invalid MAX argument".to_string())),
                    };

                    let mut max_val: Option<Value> = None;
                    for row in rows {
                        let val = evaluate_expr_context(arg_expr, ctx, row)?;
                        if !val.is_null() {
                            max_val = match max_val {
                                None => Some(val),
                                Some(curr) => {
                                    if val > curr {
                                        Some(val)
                                    } else {
                                        Some(curr)
                                    }
                                }
                            };
                        }
                    }
                    Ok(max_val.unwrap_or(Value::Null))
                }
                _ => {
                    if rows.is_empty() {
                        Ok(Value::Null)
                    } else {
                        evaluate_expr_context(expr, ctx, &rows[0])
                    }
                }
            }
        }
        Expr::Nested(inner) => evaluate_aggregate_expr(inner, ctx, rows),
        Expr::BinaryOp { left, op, right } => {
            let l = evaluate_aggregate_expr(left, ctx, rows)?;
            let r = evaluate_aggregate_expr(right, ctx, rows)?;
            crate::expression::evaluate_binary_op(&l, op, &r)
        }
        _ => {
            if rows.is_empty() {
                Ok(Value::Null)
            } else {
                evaluate_expr_context(expr, ctx, &rows[0])
            }
        }
    }
}

pub(crate) fn add_values(left: &Value, right: &Value) -> H2Result<Value> {
    match (left, right) {
        (Value::Integer(a), Value::Integer(b)) => Ok(Value::Integer(a.wrapping_add(*b))),
        (Value::BigInt(a), Value::BigInt(b)) => Ok(Value::BigInt(a.wrapping_add(*b))),
        (Value::Decimal(a), Value::Decimal(b)) => Ok(Value::Decimal(*a + *b)),
        (Value::Double(a), Value::Double(b)) => Ok(Value::Double(a + b)),
        _ => {
            if let (Some(a), Some(b)) = (left.to_f64(), right.to_f64()) {
                Ok(Value::Double(a + b))
            } else {
                Err(H2Error::TypeError(format!("Cannot add {:?} and {:?}", left, right)))
            }
        }
    }
}

pub(crate) fn value_to_sql_expr(val: Value) -> Expr {
    match val {
        Value::Null => Expr::Value(sqlparser::ast::Value::Null),
        Value::Boolean(b) => Expr::Value(sqlparser::ast::Value::Boolean(b)),
        Value::TinyInt(n) => Expr::Value(sqlparser::ast::Value::Number(n.to_string(), false)),
        Value::SmallInt(n) => Expr::Value(sqlparser::ast::Value::Number(n.to_string(), false)),
        Value::Integer(n) => Expr::Value(sqlparser::ast::Value::Number(n.to_string(), false)),
        Value::BigInt(n) => Expr::Value(sqlparser::ast::Value::Number(n.to_string(), false)),
        Value::Float(f) => Expr::Value(sqlparser::ast::Value::Number(f.to_string(), false)),
        Value::Double(d) => Expr::Value(sqlparser::ast::Value::Number(d.to_string(), false)),
        Value::Decimal(d) => Expr::Value(sqlparser::ast::Value::Number(d.to_string(), false)),
        Value::String(s) => Expr::Value(sqlparser::ast::Value::SingleQuotedString(s)),
        _ => Expr::Value(sqlparser::ast::Value::SingleQuotedString(val.to_string())),
    }
}

