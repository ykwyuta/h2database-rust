use sqlparser::ast::{
    Expr, FunctionArg, FunctionArgExpr, GroupByExpr, JoinConstraint, JoinOperator,
    Query, SelectItem, SetExpr, Statement, TableFactor,
};

use std::sync::Arc;

use h2_mvstore::{MVStore, Transaction, TransactionStore};
use h2_types::{H2Error, H2Result, Value};
use crate::catalog::Catalog;
use crate::expression::{
    evaluate_expr, evaluate_expr_context, evaluate_literal_or_unary, ColumnBinding, RowContext,
};
use crate::parser::{extract_create_table, parse_sql};
use crate::row::Row;


#[derive(Debug, Clone)]
pub enum ExecutionResult {
    Ddl,
    Dml { affected_rows: u64 },
    Query { columns: Vec<String>, rows: Vec<Row> },
}

pub struct SQLEngine {
    store: Arc<MVStore>,
    tx_store: Arc<TransactionStore>,
    catalog: Arc<Catalog>,
}

impl SQLEngine {
    pub fn new(store: Arc<MVStore>) -> H2Result<Self> {
        let tx_store = TransactionStore::new(Arc::clone(&store));
        let catalog = Arc::new(Catalog::new(Arc::clone(&store))?);
        Ok(Self {
            store,
            tx_store,
            catalog,
        })
    }

    pub fn store(&self) -> &Arc<MVStore> {
        &self.store
    }

    pub fn tx_store(&self) -> &Arc<TransactionStore> {
        &self.tx_store
    }

    pub fn catalog(&self) -> &Arc<Catalog> {
        &self.catalog
    }

    /// 暗黙トランザクション（Auto-commit）でSQLを実行
    pub fn execute(&self, sql: &str) -> H2Result<ExecutionResult> {
        let trimmed = sql.trim().trim_end_matches(';').trim();
        if trimmed.eq_ignore_ascii_case("VACUUM") {
            self.store.compact()?;
            return Ok(ExecutionResult::Ddl);
        }

        let tx = self.tx_store.begin();
        let result = self.execute_with_tx(&tx, sql)?;
        tx.commit()?;
        Ok(result)
    }

    /// 明示的トランザクションコンテキストでSQLを実行
    pub fn execute_with_tx(&self, tx: &Transaction, sql: &str) -> H2Result<ExecutionResult> {
        let trimmed = sql.trim().trim_end_matches(';').trim();
        if trimmed.eq_ignore_ascii_case("VACUUM") {
            self.store.compact()?;
            return Ok(ExecutionResult::Ddl);
        }

        let statements = parse_sql(sql)?;
        let mut last_result = ExecutionResult::Ddl;

        for stmt in statements {
            last_result = self.execute_statement(tx, stmt)?;
        }

        Ok(last_result)
    }

    fn execute_statement(&self, tx: &Transaction, stmt: Statement) -> H2Result<ExecutionResult> {
        match stmt {
            Statement::CreateTable(create_table) => {
                let table_def = extract_create_table(
                    &create_table.name,
                    &create_table.columns,
                    &create_table.constraints,
                )?;
                self.catalog.create_table(table_def)?;
                self.store.commit()?;
                Ok(ExecutionResult::Ddl)
            }
            Statement::Insert(insert) => {
                let table_name = insert.table_name.to_string();
                let table_def = self.catalog.get_table(&table_name).ok_or_else(|| {
                    H2Error::Catalog(format!("Table '{}' not found", table_name))
                })?;

                let map_name = format!("tbl_{}", table_name.to_lowercase());

                let mut affected_rows = 0;
                if let Some(source) = insert.source {
                    if let SetExpr::Values(values) = *source.body {
                        for row_exprs in values.rows {
                            if row_exprs.len() != table_def.columns.len() {
                                return Err(H2Error::Execution(format!(
                                    "Column count mismatch: expected {}, got {}",
                                    table_def.columns.len(),
                                    row_exprs.len()
                                )));
                            }

                            let mut row_values = Vec::with_capacity(row_exprs.len());
                            for (col_idx, expr) in row_exprs.iter().enumerate() {
                                let val = evaluate_literal_or_unary(expr)?;
                                let casted = val.cast_to(&table_def.columns[col_idx].data_type)?;
                                row_values.push(casted);
                            }


                            let row_id = self.catalog.allocate_row_id(&table_name)?;
                            let row = Row::new(row_values);
                            tx.put(&map_name, row_id.to_le_bytes().to_vec(), row.to_bytes()?)?;
                            affected_rows += 1;
                        }
                    }
                }

                Ok(ExecutionResult::Dml { affected_rows })
            }
            Statement::Delete(delete) => {
                let from_table = match &delete.from {
                    sqlparser::ast::FromTable::WithFromKeyword(tables) => &tables[0],
                    sqlparser::ast::FromTable::WithoutKeyword(tables) => &tables[0],
                };
                let table_name = match &from_table.relation {
                    TableFactor::Table { name, .. } => name.to_string(),
                    _ => return Err(H2Error::Execution("Complex table factors not supported".to_string())),
                };

                let table_def = self.catalog.get_table(&table_name).ok_or_else(|| {
                    H2Error::Catalog(format!("Table '{}' not found", table_name))
                })?;

                let map_name = format!("tbl_{}", table_name.to_lowercase());
                let entries = tx.scan_visible(&map_name)?;
                let mut affected_rows = 0;

                for (key, val_bytes) in entries {
                    let row = Row::from_bytes(&val_bytes)?;
                    let matches = if let Some(selection) = &delete.selection {
                        match evaluate_expr(selection, &table_def, &row)? {
                            Value::Boolean(b) => b,
                            _ => false,
                        }
                    } else {
                        true
                    };

                    if matches {
                        tx.remove(&map_name, &key)?;
                        affected_rows += 1;
                    }
                }

                Ok(ExecutionResult::Dml { affected_rows })
            }
            Statement::Update { table, assignments, selection, .. } => {
                let table_name = match &table.relation {
                    TableFactor::Table { name, .. } => name.to_string(),
                    _ => return Err(H2Error::Execution("Complex table factors in UPDATE not supported".to_string())),
                };

                let table_def = self.catalog.get_table(&table_name).ok_or_else(|| {
                    H2Error::Catalog(format!("Table '{}' not found", table_name))
                })?;

                let map_name = format!("tbl_{}", table_name.to_lowercase());
                let entries = tx.scan_visible(&map_name)?;
                let mut affected_rows = 0;

                for (key, val_bytes) in entries {
                    let mut row = Row::from_bytes(&val_bytes)?;
                    let matches = if let Some(sel) = &selection {
                        match evaluate_expr(sel, &table_def, &row)? {
                            Value::Boolean(b) => b,
                            _ => false,
                        }
                    } else {
                        true
                    };

                    if matches {
                        for assignment in &assignments {
                            let col_name = match &assignment.target {
                                sqlparser::ast::AssignmentTarget::ColumnName(object_name) => object_name.to_string(),
                                _ => return Err(H2Error::Execution("Unsupported assignment target".to_string())),
                            };
                            let col_idx = table_def.column_index(&col_name).ok_or_else(|| {
                                H2Error::Execution(format!("Column '{}' not found in table '{}'", col_name, table_name))
                            })?;
                            let new_val = evaluate_expr(&assignment.value, &table_def, &row)?;
                            let casted = new_val.cast_to(&table_def.columns[col_idx].data_type)?;
                            row.values[col_idx] = casted;

                        }

                        tx.put(&map_name, key, row.to_bytes()?)?;
                        affected_rows += 1;
                    }
                }

                Ok(ExecutionResult::Dml { affected_rows })
            }
            Statement::Query(query) => self.execute_query(tx, *query),
            _ => Err(H2Error::Execution(format!("Unsupported statement: {:?}", stmt))),
        }
    }

    fn execute_query(&self, tx: &Transaction, query: Query) -> H2Result<ExecutionResult> {
        let SetExpr::Select(select) = *query.body else {
            return Err(H2Error::Execution("Only simple SELECT queries are supported".to_string()));
        };

        if select.from.is_empty() {
            return Err(H2Error::Execution("SELECT without FROM not supported yet".to_string()));
        }

        let from_table = &select.from[0];
        let (base_table_name, base_table_alias) = match &from_table.relation {
            TableFactor::Table { name, alias, .. } => (
                name.to_string(),
                alias.as_ref().map(|a| a.name.value.clone()),
            ),
            _ => return Err(H2Error::Execution("Complex table factors not supported".to_string())),
        };

        let base_table_def = self.catalog.get_table(&base_table_name).ok_or_else(|| {
            H2Error::Catalog(format!("Table '{}' not found", base_table_name))
        })?;

        let map_name = format!("tbl_{}", base_table_name.to_lowercase());
        let entries = tx.scan_visible(&map_name)?;
        let mut current_rows = Vec::with_capacity(entries.len());
        for (_k, val_bytes) in entries {
            current_rows.push(Row::from_bytes(&val_bytes)?);
        }

        let mut ctx = RowContext::from_table_def(&base_table_def, base_table_alias.as_deref());

        // JOIN の処理
        for join in &from_table.joins {
            let (join_table_name, join_table_alias) = match &join.relation {
                TableFactor::Table { name, alias, .. } => (
                    name.to_string(),
                    alias.as_ref().map(|a| a.name.value.clone()),
                ),
                _ => return Err(H2Error::Execution("Complex join relations not supported".to_string())),
            };

            let join_table_def = self.catalog.get_table(&join_table_name).ok_or_else(|| {
                H2Error::Catalog(format!("Table '{}' not found in JOIN", join_table_name))
            })?;

            let join_map_name = format!("tbl_{}", join_table_name.to_lowercase());
            let join_entries = tx.scan_visible(&join_map_name)?;
            let mut join_rows = Vec::with_capacity(join_entries.len());
            for (_k, val_bytes) in join_entries {
                join_rows.push(Row::from_bytes(&val_bytes)?);
            }

            let base_idx = ctx.columns.len();
            ctx.append_table(&join_table_def, join_table_alias.as_deref(), base_idx);

            match &join.join_operator {
                JoinOperator::Inner(JoinConstraint::On(on_expr)) => {
                    let mut new_rows = Vec::new();
                    for l in &current_rows {
                        for r in &join_rows {
                            let mut vals = l.values.clone();
                            vals.extend(r.values.clone());
                            let combined_row = Row::new(vals);
                            if let Value::Boolean(true) = evaluate_expr_context(on_expr, &ctx, &combined_row)? {
                                new_rows.push(combined_row);
                            }
                        }
                    }
                    current_rows = new_rows;
                }
                JoinOperator::LeftOuter(JoinConstraint::On(on_expr)) => {
                    let mut new_rows = Vec::new();
                    let right_null_vals = vec![Value::Null; join_table_def.columns.len()];
                    for l in &current_rows {
                        let mut matched_any = false;
                        for r in &join_rows {
                            let mut vals = l.values.clone();
                            vals.extend(r.values.clone());
                            let combined_row = Row::new(vals);
                            if let Value::Boolean(true) = evaluate_expr_context(on_expr, &ctx, &combined_row)? {
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
                    current_rows = new_rows;
                }
                _ => return Err(H2Error::Execution(format!("Unsupported join operator: {:?}", join.join_operator))),
            }
        }

        // WHERE 句のフィルタリング
        let filtered_rows = if let Some(selection) = &select.selection {
            let mut matched = Vec::new();
            for row in current_rows {
                if let Value::Boolean(true) = evaluate_expr_context(selection, &ctx, &row)? {
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
                            evaluate_aggregate_expr(expr, &ctx, &group_rows)?
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
                    let having_val = evaluate_aggregate_expr(having, &ctx, &group_rows)?;
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

            // 集約クエリの ORDER BY
            if let Some(order_by) = &query.order_by {
                let res_ctx = RowContext {
                    columns: result_columns.iter().enumerate().map(|(i, name)| ColumnBinding {
                        table_name: None,
                        table_alias: None,
                        column_name: name.clone(),
                        index: i,
                    }).collect(),
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

            let mut extended_rows = Vec::with_capacity(filtered_rows.len());
            for row in filtered_rows {
                if is_wildcard {
                    extended_rows.push(row);
                } else {
                    let mut proj_vals = Vec::with_capacity(select.projection.len());
                    for item in &select.projection {
                        let val = match item {
                            SelectItem::UnnamedExpr(expr) | SelectItem::ExprWithAlias { expr, .. } => {
                                evaluate_expr_context(expr, &ctx, &row)?
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
                extended_rows.sort_by(|a, b| {
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

        let final_rows = if let Some(lim) = limit_num {
            projected_rows.into_iter().skip(offset_num).take(lim).collect()
        } else {
            projected_rows.into_iter().skip(offset_num).collect()
        };

        Ok(ExecutionResult::Query {
            columns: result_columns,
            rows: final_rows,
        })
    }
}

fn has_aggregate_func(expr: &Expr) -> bool {
    match expr {
        Expr::Function(func) => {
            let name = func.name.to_string().to_uppercase();
            matches!(name.as_str(), "COUNT" | "SUM" | "AVG" | "MIN" | "MAX")
        }
        Expr::BinaryOp { left, right, .. } => has_aggregate_func(left) || has_aggregate_func(right),
        Expr::UnaryOp { expr, .. } | Expr::Nested(expr) => has_aggregate_func(expr),
        _ => false,
    }
}

fn get_select_item_name(item: &SelectItem) -> String {
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

fn evaluate_aggregate_expr(expr: &Expr, ctx: &RowContext, rows: &[Row]) -> H2Result<Value> {
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

fn add_values(left: &Value, right: &Value) -> H2Result<Value> {
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

