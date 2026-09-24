use sqlparser::ast::{
    BinaryOperator, Expr, FunctionArg, FunctionArgExpr, GroupByExpr,
    JoinConstraint, JoinOperator, Query, SelectItem, SetExpr, Statement, TableFactor,
};


use std::collections::HashMap;
use std::sync::Arc;

use h2_mvstore::{MVStore, Transaction, TransactionStore};
use h2_types::{H2Error, H2Result, Value};
use crate::catalog::{Catalog, ColumnDef, ForeignKeyAction, IndexDef, TableDef};
use crate::expression::{
    evaluate_expr, evaluate_expr_context, evaluate_literal_or_unary, ColumnBinding, RowContext,
};
use crate::parser::{convert_data_type, extract_create_table, parse_sql};
use crate::row::Row;

pub(crate) fn encode_composite_index_key(vals: &[Value], row_id: u64) -> Vec<u8> {
    let mut bytes = if vals.len() == 1 {
        serde_json::to_vec(&vals[0]).unwrap_or_default()
    } else {
        serde_json::to_vec(vals).unwrap_or_default()
    };
    bytes.push(0x00);
    bytes.extend_from_slice(&row_id.to_be_bytes());
    bytes
}

pub(crate) fn encode_index_key(val: &Value, row_id: u64) -> Vec<u8> {
    encode_composite_index_key(std::slice::from_ref(val), row_id)
}

pub(crate) fn decode_composite_index_key(bytes: &[u8]) -> Option<(Vec<Value>, u64)> {
    if bytes.len() < 9 {
        return None;
    }
    let val_bytes = &bytes[..bytes.len() - 9];
    let row_id_bytes = &bytes[bytes.len() - 8..];
    let row_id = u64::from_be_bytes(row_id_bytes.try_into().ok()?);
    if let Ok(vals) = serde_json::from_slice::<Vec<Value>>(val_bytes) {
        return Some((vals, row_id));
    }
    if let Ok(val) = serde_json::from_slice::<Value>(val_bytes) {
        return Some((vec![val], row_id));
    }
    None
}

pub(crate) fn decode_index_key(bytes: &[u8]) -> Option<(Value, u64)> {
    let (vals, row_id) = decode_composite_index_key(bytes)?;
    vals.into_iter().next().map(|v| (v, row_id))
}

pub(crate) fn get_index_values(table_def: &TableDef, idx: &IndexDef, row: &Row) -> Option<Vec<Value>> {
    let mut vals = Vec::with_capacity(idx.columns.len());
    for col_name in &idx.columns {
        let c_idx = table_def.column_index(col_name)?;
        vals.push(row.values.get(c_idx).cloned().unwrap_or(Value::Null));
    }
    Some(vals)
}

pub(crate) fn project_returning(
    table_def: &TableDef,
    rows: &[Row],
    returning: &[SelectItem],
) -> H2Result<(Vec<String>, Vec<Row>)> {
    let ctx = RowContext::from_table_def(table_def, None);
    let mut columns = Vec::new();
    let mut is_wildcard = false;

    for item in returning {
        match item {
            SelectItem::Wildcard(_) => {
                is_wildcard = true;
                break;
            }
            SelectItem::UnnamedExpr(expr) => {
                columns.push(match expr {
                    Expr::Identifier(ident) => ident.value.clone(),
                    _ => expr.to_string(),
                });
            }
            SelectItem::ExprWithAlias { alias, .. } => {
                columns.push(alias.value.clone());
            }
            _ => return Err(H2Error::Execution("Unsupported RETURNING expression".to_string())),
        }
    }

    if is_wildcard {
        let cols = table_def.columns.iter().map(|c| c.name.clone()).collect();
        return Ok((cols, rows.to_vec()));
    }

    let mut res_rows = Vec::with_capacity(rows.len());
    for r in rows {
        let mut vals = Vec::with_capacity(returning.len());
        for item in returning {
            match item {
                SelectItem::UnnamedExpr(expr) => {
                    let v = evaluate_expr_context(expr, &ctx, r)?;
                    vals.push(v);
                }
                SelectItem::ExprWithAlias { expr, .. } => {
                    let v = evaluate_expr_context(expr, &ctx, r)?;
                    vals.push(v);
                }
                _ => {}
            }
        }
        res_rows.push(Row::new(vals));
    }

    Ok((columns, res_rows))
}

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

    fn resolve_view_query(
        &self,
        tx: &Transaction,
        view: &crate::catalog::ViewDef,
        alias: Option<String>,
        current_ctes: &HashMap<String, (TableDef, Vec<Row>)>,
    ) -> H2Result<(TableDef, Vec<Row>, Option<String>)> {
        let dialect = sqlparser::dialect::GenericDialect {};
        let ast = sqlparser::parser::Parser::parse_sql(&dialect, &view.query_sql)
            .map_err(|e| H2Error::SqlParse(e.to_string()))?;
        if let Some(Statement::Query(view_q)) = ast.into_iter().next() {
            let sub_res = self.execute_query_with_ctes(tx, *view_q, current_ctes)?;
            let (cols, rows) = match sub_res {
                ExecutionResult::Query { columns, rows } => (columns, rows),
                _ => return Err(H2Error::Execution("View query must return rows".to_string())),
            };
            let final_cols = if !view.columns.is_empty() && view.columns.len() == cols.len() {
                view.columns.clone()
            } else {
                cols
            };
            let col_defs = final_cols.into_iter().map(|c| ColumnDef::new(
                c,
                h2_types::DataType::VarChar(None),
                true,
                false,
            )).collect();
            let effective_name = alias.clone().unwrap_or_else(|| view.name.clone());
            let t_def = crate::catalog::TableDef::new(effective_name, col_defs);
            Ok((t_def, rows, alias))
        } else {
            Err(H2Error::Execution(format!("Invalid query in view '{}'", view.name)))
        }
    }

    fn resolve_table_factor(
        &self,
        tx: &Transaction,
        relation: &TableFactor,
        current_ctes: &HashMap<String, (TableDef, Vec<Row>)>,
    ) -> H2Result<(TableDef, Vec<Row>, Option<String>)> {
        match relation {
            TableFactor::Derived { subquery, alias, .. } => {
                let sub_alias = alias.as_ref().map(|a| a.name.value.clone()).unwrap_or_else(|| "subquery".to_string());
                let sub_res = self.execute_query_with_ctes(tx, *subquery.clone(), current_ctes)?;
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
                Ok((t_def, rows, Some(sub_alias)))
            }
            TableFactor::Table { name, alias, .. } => {
                let table_name = name.to_string();
                let table_alias = alias.as_ref().map(|a| a.name.value.clone());

                if let Some((cte_def, cte_rows)) = current_ctes.get(&table_name.to_lowercase()) {
                    let mut t_def = cte_def.clone();
                    if let Some(ref a) = table_alias {
                        t_def.name = a.clone();
                    }
                    Ok((t_def, cte_rows.clone(), table_alias))
                } else if let Some(view) = self.catalog.get_view(&table_name) {
                    self.resolve_view_query(tx, &view, table_alias, current_ctes)
                } else {
                    let is_info_tables = table_name.eq_ignore_ascii_case("information_schema.tables")
                        || table_name.eq_ignore_ascii_case("tables");
                    let is_info_columns = table_name.eq_ignore_ascii_case("information_schema.columns")
                        || table_name.eq_ignore_ascii_case("columns");
                    let is_info_schemata = table_name.eq_ignore_ascii_case("information_schema.schemata")
                        || table_name.eq_ignore_ascii_case("schemata");

                    if is_info_tables {
                        let t_def = crate::catalog::TableDef::new(
                            table_name.clone(),
                            vec![
                                crate::catalog::ColumnDef::new("table_name", h2_types::DataType::VarChar(None), false, true),
                                crate::catalog::ColumnDef::new("columns_count", h2_types::DataType::Integer, false, false),
                            ],
                        );
                        let tables = self.catalog.all_tables();
                        let rows = tables.into_iter().map(|t| Row::new(vec![
                            Value::String(t.name),
                            Value::Integer(t.columns.len() as i32),
                        ])).collect();
                        Ok((t_def, rows, table_alias))
                    } else if is_info_columns {
                        let t_def = crate::catalog::TableDef::new(
                            table_name.clone(),
                            vec![
                                crate::catalog::ColumnDef::new("table_name", h2_types::DataType::VarChar(None), false, true),
                                crate::catalog::ColumnDef::new("column_name", h2_types::DataType::VarChar(None), false, false),
                                crate::catalog::ColumnDef::new("data_type", h2_types::DataType::VarChar(None), false, false),
                                crate::catalog::ColumnDef::new("is_nullable", h2_types::DataType::Boolean, false, false),
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
                        Ok((t_def, rows, table_alias))
                    } else if is_info_schemata {
                        let t_def = crate::catalog::TableDef::new(
                            table_name.clone(),
                            vec![
                                crate::catalog::ColumnDef::new("schema_name", h2_types::DataType::VarChar(None), false, true),
                            ],
                        );
                        let mut schemas = self.catalog.get_schemas();
                        schemas.sort();
                        let rows = schemas.into_iter().map(|s| Row::new(vec![Value::String(s)])).collect();
                        Ok((t_def, rows, table_alias))
                    } else {
                        let table_def = self.catalog.get_table(&table_name).ok_or_else(|| {
                            H2Error::Catalog(format!("Table '{}' not found", table_name))
                        })?;
                        let map_name = table_def.map_name();
                        let entries = tx.scan_visible(&map_name)?;
                        let mut rows = Vec::with_capacity(entries.len());
                        for (_k, val_bytes) in entries {
                            let mut row = Row::from_bytes(&val_bytes)?;
                            table_def.align_row(&mut row);
                            rows.push(row);
                        }
                        Ok((table_def, rows, table_alias))
                    }
                }
            }
            _ => Err(H2Error::Execution("Complex table factors not supported".to_string())),
        }
    }

fn is_join_match(
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

fn apply_join(
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

    fn evaluate_table_with_joins(
        &self,
        tx: &Transaction,
        tbl_with_joins: &sqlparser::ast::TableWithJoins,
        current_ctes: &HashMap<String, (TableDef, Vec<Row>)>,
    ) -> H2Result<(RowContext, Vec<Row>)> {
        let (base_def, base_rows, base_alias) = self.resolve_table_factor(tx, &tbl_with_joins.relation, current_ctes)?;
        let mut ctx = RowContext::from_table_def(&base_def, base_alias.as_deref());
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

    fn execute_statement(&self, tx: &Transaction, stmt: Statement) -> H2Result<ExecutionResult> {
        h2_types::check_query_timeout()?;
        match stmt {
            Statement::CreateView {
                or_replace,
                name,
                columns,
                query,
                ..
            } => {
                let view_name = name.to_string();
                let col_names = columns.into_iter().map(|c| c.name.value).collect();
                let query_sql = query.to_string();
                let view_def = crate::catalog::ViewDef {
                    name: view_name,
                    query_sql,
                    columns: col_names,
                };
                self.catalog.create_view(view_def, or_replace)?;
                self.store.commit()?;
                Ok(ExecutionResult::Ddl)
            }
            Statement::CreateSchema { schema_name, if_not_exists } => {
                let schema_str = schema_name.to_string();
                let res = self.catalog.create_schema(&schema_str);
                if let Err(e) = res {
                    if !(if_not_exists && e.to_string().contains("already exists")) {
                        return Err(e);
                    }
                }
                self.store.commit()?;
                Ok(ExecutionResult::Ddl)
            }
            Statement::CreateTable(create_table) => {
                let table_def = extract_create_table(
                    &create_table.name,
                    &create_table.columns,
                    &create_table.constraints,
                )?;
                let tbl_name = table_def.name.clone();
                let pk = table_def.primary_key.clone();
                let unique_constraints = table_def.unique_constraints.clone();

                self.catalog.create_table(table_def)?;

                // 主キー用ユニークインデックス作成
                if !pk.is_empty() {
                    let pk_index_name = format!("pk_{}", tbl_name.to_lowercase());
                    let _ = self.catalog.create_index(IndexDef {
                        name: pk_index_name,
                        table_name: tbl_name.clone(),
                        columns: pk,
                        is_unique: true,
                    });
                }

                // 一意制約用ユニークインデックス作成
                for u_def in unique_constraints {
                    let idx_name = u_def.name.unwrap_or_else(|| {
                        format!("uniq_{}_{}", tbl_name.to_lowercase(), u_def.columns.join("_").to_lowercase())
                    });
                    let _ = self.catalog.create_index(IndexDef {
                        name: idx_name,
                        table_name: tbl_name.clone(),
                        columns: u_def.columns,
                        is_unique: true,
                    });
                }

                self.store.commit()?;
                Ok(ExecutionResult::Ddl)
            }
            Statement::CreateIndex(create_index) => {
                let table_name = create_index.table_name.to_string();
                let table_def = self.catalog.get_table(&table_name).ok_or_else(|| {
                    H2Error::Catalog(format!("Table '{}' not found", table_name))
                })?;

                let index_name = create_index.name.map(|n| n.to_string()).unwrap_or_else(|| {
                    let first_col = create_index.columns.first().map(|c| c.expr.to_string()).unwrap_or_else(|| "col".to_string());
                    format!("idx_{}_{}", table_name, first_col)
                });

                let mut col_names = Vec::new();
                for col in &create_index.columns {
                    let col_name = match &col.expr {
                        Expr::Identifier(ident) => ident.value.clone(),
                        _ => return Err(H2Error::Execution("Only simple column names supported in index".to_string())),
                    };
                    if table_def.column_index(&col_name).is_none() {
                        return Err(H2Error::Catalog(format!("Column '{}' not found in table '{}'", col_name, table_name)));
                    }
                    col_names.push(col_name);
                }

                let is_unique = create_index.unique;
                let index_def = IndexDef {
                    name: index_name.clone(),
                    table_name: table_name.clone(),
                    columns: col_names.clone(),
                    is_unique,
                };

                self.catalog.create_index(index_def)?;

                // 既存行をインデックスにロード
                let idx_map_name = format!("idx_{}_{}", table_name.to_lowercase(), index_name.to_lowercase());
                let tbl_map_name = table_def.map_name();
                let entries = tx.scan_visible(&tbl_map_name)?;

                let mut col_indices = Vec::new();
                for col_name in &col_names {
                    let c_idx = table_def.column_index(col_name).unwrap();
                    col_indices.push(c_idx);
                }
                let mut seen_keys = Vec::new();

                let _is_concurrently = create_index.concurrently;
                for (k, val_bytes) in entries {
                    let mut row = Row::from_bytes(&val_bytes)?;
                    table_def.align_row(&mut row);
                    let row_id = if k.len() == 8 {
                        u64::from_le_bytes(k.as_slice().try_into().unwrap())
                    } else {
                        0
                    };
                    let vals: Vec<Value> = col_indices
                        .iter()
                        .map(|&idx| row.get(idx).cloned().unwrap_or(Value::Null))
                        .collect();

                    if is_unique && !vals.iter().any(|v| v.is_null()) {
                        if seen_keys.contains(&vals) {
                            return Err(H2Error::Execution(format!("Unique constraint violation on creating index '{}'", index_name)));
                        }
                        seen_keys.push(vals.clone());
                    }

                    let idx_key = encode_composite_index_key(&vals, row_id);
                    tx.put(&idx_map_name, idx_key, vec![])?;
                }

                self.store.commit()?;
                Ok(ExecutionResult::Ddl)
            }
            Statement::Insert(insert) => {
                let table_name = insert.table_name.to_string();
                let table_def = self.catalog.get_table(&table_name).ok_or_else(|| {
                    H2Error::Catalog(format!("Table '{}' not found", table_name))
                })?;

                let map_name = table_def.map_name();
                let indexes = self.catalog.get_table_indexes(&table_name);

                let target_indices: Vec<usize> = if insert.columns.is_empty() {
                    (0..table_def.columns.len()).collect()
                } else {
                    let mut indices = Vec::with_capacity(insert.columns.len());
                    for col in &insert.columns {
                        let col_name = col.value.as_str();
                        let idx = table_def.column_index(col_name).ok_or_else(|| {
                            H2Error::Catalog(format!("Column '{}' not found in table '{}'", col_name, table_name))
                        })?;
                        indices.push(idx);
                    }
                    indices
                };

                let rows_to_insert: Vec<Vec<Value>> = if let Some(source) = insert.source {
                    if source.with.is_none() && matches!(*source.body, SetExpr::Values(_)) {
                        if let SetExpr::Values(values) = *source.body {
                            let mut list = Vec::with_capacity(values.rows.len());
                            for row_exprs in values.rows {
                                let mut row_vals = Vec::with_capacity(row_exprs.len());
                                for expr in row_exprs {
                                    let val = evaluate_literal_or_unary(&expr)?;
                                    row_vals.push(val);
                                }
                                list.push(row_vals);
                            }
                            list
                        } else {
                            unreachable!()
                        }
                    } else {
                        // INSERT INTO ... SELECT ... または WITH ... SELECT
                        let query = *source;
                        let exec_res = self.execute_query(tx, query)?;
                        match exec_res {
                            ExecutionResult::Query { rows, .. } => rows.into_iter().map(|r| r.values).collect(),
                            _ => return Err(H2Error::Execution("INSERT source query must produce rows".to_string())),
                        }
                    }
                } else {
                    Vec::new()
                };

                let mut affected_rows = 0;
                let mut returning_rows = Vec::new();
                for raw_values in rows_to_insert {
                    if raw_values.len() != target_indices.len() {
                        return Err(H2Error::Execution(format!(
                            "Column count mismatch: expected {}, got {}",
                            target_indices.len(),
                            raw_values.len()
                        )));
                    }

                    let mut full_row_values = vec![Value::Null; table_def.columns.len()];
                    for (i, val) in raw_values.into_iter().enumerate() {
                        let col_idx = target_indices[i];
                        let casted = val.cast_to(&table_def.columns[col_idx].data_type)?;
                        full_row_values[col_idx] = casted;
                    }

                    let row = Row::new(full_row_values);

                    // 外部キー制約の検証
                    self.validate_foreign_keys_for_row(tx, &table_def, &row)?;

                    // 一意性チェックおよび競合行の検出
                    let mut conflict_info: Option<(u64, Row)> = None;
                    for idx in &indexes {
                        if let Some(vals) = get_index_values(&table_def, idx, &row) {
                            let idx_map_name = format!("idx_{}_{}", table_def.name.to_lowercase(), idx.name.to_lowercase());
                            let has_null = vals.iter().any(|v| v.is_null());
                            if idx.is_unique && !has_null {
                                let existing = tx.scan_visible(&idx_map_name)?;
                                for (k, _) in existing {
                                    if let Some((v, conflicting_row_id)) = decode_composite_index_key(&k) {
                                        if v == vals {
                                            if let Some(bytes) = tx.get(&map_name, &conflicting_row_id.to_le_bytes())? {
                                                let mut conf_row = Row::from_bytes(&bytes)?;
                                                table_def.align_row(&mut conf_row);
                                                conflict_info = Some((conflicting_row_id, conf_row));
                                                break;
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        if conflict_info.is_some() {
                            break;
                        }
                    }

                    if let Some((conflict_row_id, old_row)) = conflict_info {
                        if let Some(ref on_insert) = insert.on {
                            match on_insert {
                                sqlparser::ast::OnInsert::OnConflict(on_conflict) => {
                                    match &on_conflict.action {
                                        sqlparser::ast::OnConflictAction::DoNothing => {
                                            // 何もせずスキップ
                                            continue;
                                        }
                                        sqlparser::ast::OnConflictAction::DoUpdate(do_update) => {
                                            let mut ctx = RowContext::from_table_def(&table_def, None);
                                            let base_idx = table_def.columns.len();
                                            ctx.append_table(&table_def, Some("EXCLUDED"), base_idx);

                                            let mut combined_values = old_row.values.clone();
                                            combined_values.extend(row.values.clone());
                                            let combined_row = Row::new(combined_values);

                                            if let Some(ref sel) = do_update.selection {
                                                match evaluate_expr_context(sel, &ctx, &combined_row)? {
                                                    Value::Boolean(true) => {}
                                                    _ => continue,
                                                }
                                            }

                                            let mut updated_row = old_row.clone();
                                            for assignment in &do_update.assignments {
                                                let col_name = match &assignment.target {
                                                    sqlparser::ast::AssignmentTarget::ColumnName(object_name) => object_name.to_string(),
                                                    _ => return Err(H2Error::Execution("Unsupported assignment target".to_string())),
                                                };
                                                let col_idx = table_def.column_index(&col_name).ok_or_else(|| {
                                                    H2Error::Execution(format!("Column '{}' not found in table '{}'", col_name, table_name))
                                                })?;
                                                let new_val = evaluate_expr_context(&assignment.value, &ctx, &combined_row)?;
                                                let casted = new_val.cast_to(&table_def.columns[col_idx].data_type)?;
                                                updated_row.values[col_idx] = casted;
                                            }

                                            self.handle_foreign_keys_on_update(tx, &table_name, &old_row, &updated_row)?;
                                            self.validate_foreign_keys_for_row(tx, &table_def, &updated_row)?;

                                            // インデックス更新
                                            for idx in &indexes {
                                                if let (Some(old_vals), Some(new_vals)) = (
                                                    get_index_values(&table_def, idx, &old_row),
                                                    get_index_values(&table_def, idx, &updated_row),
                                                ) {
                                                    let idx_map_name = format!("idx_{}_{}", table_def.name.to_lowercase(), idx.name.to_lowercase());
                                                    let has_null = new_vals.iter().any(|v| v.is_null());

                                                    if idx.is_unique && !has_null && old_vals != new_vals {
                                                        let existing = tx.scan_visible(&idx_map_name)?;
                                                        for (k, _) in existing {
                                                            if let Some((v, r_id)) = decode_composite_index_key(&k) {
                                                                if v == new_vals && r_id != conflict_row_id {
                                                                    return Err(H2Error::Execution(format!(
                                                                        "Unique constraint violation on index '{}': duplicate value {:?}",
                                                                        idx.name, new_vals
                                                                    )));
                                                                }
                                                            }
                                                        }
                                                    }

                                                    let old_key = encode_composite_index_key(&old_vals, conflict_row_id);
                                                    tx.remove(&idx_map_name, &old_key)?;
                                                    let new_key = encode_composite_index_key(&new_vals, conflict_row_id);
                                                    tx.put(&idx_map_name, new_key, vec![])?;
                                                }
                                            }

                                            tx.put(&map_name, conflict_row_id.to_le_bytes().to_vec(), updated_row.to_bytes()?)?;
                                            affected_rows += 1;
                                            returning_rows.push(updated_row);
                                            continue;
                                        }
                                    }
                                }
                                sqlparser::ast::OnInsert::DuplicateKeyUpdate(assignments) => {
                                    let mut updated_row = old_row.clone();
                                    for assignment in assignments {
                                        let col_name = match &assignment.target {
                                            sqlparser::ast::AssignmentTarget::ColumnName(object_name) => object_name.to_string(),
                                            _ => return Err(H2Error::Execution("Unsupported assignment target".to_string())),
                                        };
                                        let col_idx = table_def.column_index(&col_name).ok_or_else(|| {
                                            H2Error::Execution(format!("Column '{}' not found in table '{}'", col_name, table_name))
                                        })?;
                                        let new_val = evaluate_expr(&assignment.value, &table_def, &old_row)?;
                                        let casted = new_val.cast_to(&table_def.columns[col_idx].data_type)?;
                                        updated_row.values[col_idx] = casted;
                                    }

                                    self.handle_foreign_keys_on_update(tx, &table_name, &old_row, &updated_row)?;
                                    self.validate_foreign_keys_for_row(tx, &table_def, &updated_row)?;

                                    for idx in &indexes {
                                        if let (Some(old_vals), Some(new_vals)) = (
                                            get_index_values(&table_def, idx, &old_row),
                                            get_index_values(&table_def, idx, &updated_row),
                                        ) {
                                            let idx_map_name = format!("idx_{}_{}", table_def.name.to_lowercase(), idx.name.to_lowercase());
                                            let old_key = encode_composite_index_key(&old_vals, conflict_row_id);
                                            tx.remove(&idx_map_name, &old_key)?;
                                            let new_key = encode_composite_index_key(&new_vals, conflict_row_id);
                                            tx.put(&idx_map_name, new_key, vec![])?;
                                        }
                                    }

                                    tx.put(&map_name, conflict_row_id.to_le_bytes().to_vec(), updated_row.to_bytes()?)?;
                                    affected_rows += 1;
                                    returning_rows.push(updated_row);
                                    continue;
                                }
                                _ => return Err(H2Error::Execution("Unsupported ON INSERT clause".to_string())),
                            }
                        } else {
                            return Err(H2Error::Execution(format!(
                                "Unique constraint violation on table '{}': duplicate key found",
                                table_name
                            )));
                        }
                    }

                    // 競合なし: 通常挿入
                    let row_id = self.catalog.allocate_row_id(&table_name)?;
                    for idx in &indexes {
                        if let Some(vals) = get_index_values(&table_def, idx, &row) {
                            let idx_map_name = format!("idx_{}_{}", table_def.name.to_lowercase(), idx.name.to_lowercase());
                            let idx_key = encode_composite_index_key(&vals, row_id);
                            tx.put(&idx_map_name, idx_key, vec![])?;
                        }
                    }

                    tx.put(&map_name, row_id.to_le_bytes().to_vec(), row.to_bytes()?)?;
                    affected_rows += 1;
                    returning_rows.push(row);
                }

                if let Some(ref returning) = insert.returning {
                    if !returning.is_empty() {
                        let (cols, res_rows) = project_returning(&table_def, &returning_rows, returning)?;
                        return Ok(ExecutionResult::Query { columns: cols, rows: res_rows });
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
                let target_alias = match &from_table.relation {
                    TableFactor::Table { alias, .. } => alias.as_ref().map(|a| a.name.value.clone()),
                    _ => None,
                };

                let table_def = self.catalog.get_table(&table_name).ok_or_else(|| {
                    H2Error::Catalog(format!("Table '{}' not found", table_name))
                })?;

                let using_data: Option<(RowContext, Vec<Row>)> = if let Some(ref using_tables) = delete.using {
                    if !using_tables.is_empty() {
                        let mut combined_ctx = RowContext::new();
                        let mut combined_rows = vec![Row::new(vec![])];

                        for u_twj in using_tables {
                            let (u_ctx, u_rows) = self.evaluate_table_with_joins(tx, u_twj, &HashMap::new())?;
                            let base_idx = combined_ctx.columns.len();
                            for b in &u_ctx.columns {
                                let mut b_clone = b.clone();
                                b_clone.index += base_idx;
                                combined_ctx.columns.push(b_clone);
                            }

                            let mut new_rows = Vec::new();
                            for cr in &combined_rows {
                                for ur in &u_rows {
                                    let mut vals = cr.values.clone();
                                    vals.extend(ur.values.clone());
                                    new_rows.push(Row::new(vals));
                                }
                            }
                            combined_rows = new_rows;
                        }
                        Some((combined_ctx, combined_rows))
                    } else {
                        None
                    }
                } else {
                    None
                };

                let map_name = table_def.map_name();
                let indexes = self.catalog.get_table_indexes(&table_name);
                let entries = tx.scan_visible(&map_name)?;
                let mut affected_rows = 0;
                let mut deleted_rows = Vec::new();
                let target_ctx = RowContext::from_table_def(&table_def, target_alias.as_deref());

                for (key, val_bytes) in entries {
                    let mut row = Row::from_bytes(&val_bytes)?;
                    table_def.align_row(&mut row);

                    let matches = if let Some((ref u_ctx, ref u_rows)) = using_data {
                        let mut merged_ctx = target_ctx.clone();
                        for b in &u_ctx.columns {
                            let mut b_clone = b.clone();
                            b_clone.index += target_ctx.columns.len();
                            merged_ctx.columns.push(b_clone);
                        }

                        let mut matched = false;
                        for ur in u_rows {
                            let mut vals = row.values.clone();
                            vals.extend(ur.values.clone());
                            let combined = Row::new(vals);

                            let is_match = if let Some(selection) = &delete.selection {
                                match evaluate_expr_context(selection, &merged_ctx, &combined)? {
                                    Value::Boolean(b) => b,
                                    _ => false,
                                }
                            } else {
                                true
                            };

                            if is_match {
                                matched = true;
                                break;
                            }
                        }
                        matched
                    } else {
                        if let Some(selection) = &delete.selection {
                            match evaluate_expr_context(selection, &target_ctx, &row)? {
                                Value::Boolean(b) => b,
                                _ => false,
                            }
                        } else {
                            true
                        }
                    };

                    if matches {
                        let row_id = if key.len() == 8 {
                            u64::from_le_bytes(key.as_slice().try_into().unwrap())
                        } else {
                            0
                        };

                        // 外部キー連動チェック・処理（ON DELETE RESTRICT / CASCADE / SET NULL）
                        self.handle_foreign_keys_on_delete(tx, &table_name, &row)?;

                        // インデックスからキー削除
                        for idx in &indexes {
                            if let Some(vals) = get_index_values(&table_def, idx, &row) {
                                let idx_map_name = format!("idx_{}_{}", table_name.to_lowercase(), idx.name.to_lowercase());
                                let idx_key = encode_composite_index_key(&vals, row_id);
                                tx.remove(&idx_map_name, &idx_key)?;
                            }
                        }

                        tx.remove(&map_name, &key)?;
                        affected_rows += 1;
                        deleted_rows.push(row);
                    }
                }

                if let Some(ref ret) = delete.returning {
                    if !ret.is_empty() {
                        let (cols, r_rows) = project_returning(&table_def, &deleted_rows, ret)?;
                        return Ok(ExecutionResult::Query { columns: cols, rows: r_rows });
                    }
                }

                Ok(ExecutionResult::Dml { affected_rows })
            }
            Statement::Update { table, assignments, from, selection, returning, .. } => {
                let table_name = match &table.relation {
                    TableFactor::Table { name, .. } => name.to_string(),
                    _ => return Err(H2Error::Execution("Complex table factors in UPDATE not supported".to_string())),
                };
                let target_alias = match &table.relation {
                    TableFactor::Table { alias, .. } => alias.as_ref().map(|a| a.name.value.clone()),
                    _ => None,
                };

                let table_def = self.catalog.get_table(&table_name).ok_or_else(|| {
                    H2Error::Catalog(format!("Table '{}' not found", table_name))
                })?;

                let from_data: Option<(RowContext, Vec<Row>)> = if let Some(ref from_twj) = from {
                    Some(self.evaluate_table_with_joins(tx, from_twj, &HashMap::new())?)
                } else {
                    None
                };

                let map_name = table_def.map_name();
                let indexes = self.catalog.get_table_indexes(&table_name);
                let entries = tx.scan_visible(&map_name)?;
                let mut affected_rows = 0;
                let mut updated_rows = Vec::new();
                let mut updated_keys = std::collections::HashSet::new();
                let target_ctx = RowContext::from_table_def(&table_def, target_alias.as_deref());

                for (key, val_bytes) in entries {
                    if updated_keys.contains(&key) {
                        continue;
                    }
                    let mut row = Row::from_bytes(&val_bytes)?;
                    table_def.align_row(&mut row);

                    let row_id = if key.len() == 8 {
                        u64::from_le_bytes(key.as_slice().try_into().unwrap())
                    } else {
                        0
                    };

                    if let Some((ref f_ctx, ref f_rows)) = from_data {
                        let mut merged_ctx = target_ctx.clone();
                        for b in &f_ctx.columns {
                            let mut b_clone = b.clone();
                            b_clone.index += target_ctx.columns.len();
                            merged_ctx.columns.push(b_clone);
                        }

                        let mut matched = false;
                        let mut matched_combined = None;
                        for f_row in f_rows {
                            let mut vals = row.values.clone();
                            vals.extend(f_row.values.clone());
                            let combined = Row::new(vals);

                            let is_match = if let Some(sel) = &selection {
                                match evaluate_expr_context(sel, &merged_ctx, &combined)? {
                                    Value::Boolean(b) => b,
                                    _ => false,
                                }
                            } else {
                                true
                            };

                            if is_match {
                                matched = true;
                                matched_combined = Some(combined);
                                break;
                            }
                        }

                        if matched {
                            let combined_row = matched_combined.unwrap();
                            let old_row = row.clone();

                            for assignment in &assignments {
                                let col_name = match &assignment.target {
                                    sqlparser::ast::AssignmentTarget::ColumnName(object_name) => object_name.to_string(),
                                    _ => return Err(H2Error::Execution("Unsupported assignment target".to_string())),
                                };
                                let col_idx = table_def.column_index(&col_name).ok_or_else(|| {
                                    H2Error::Execution(format!("Column '{}' not found in table '{}'", col_name, table_name))
                                })?;
                                let new_val = evaluate_expr_context(&assignment.value, &merged_ctx, &combined_row)?;
                                let casted = new_val.cast_to(&table_def.columns[col_idx].data_type)?;
                                row.values[col_idx] = casted;
                            }

                            // 外部キー連動チェック・処理（親行としての更新）
                            self.handle_foreign_keys_on_update(tx, &table_name, &old_row, &row)?;

                            // 外部キー整合性チェック（子行としての更新）
                            self.validate_foreign_keys_for_row(tx, &table_def, &row)?;

                            // インデックス更新
                            for idx in &indexes {
                                if let (Some(old_vals), Some(new_vals)) = (
                                    get_index_values(&table_def, idx, &old_row),
                                    get_index_values(&table_def, idx, &row),
                                ) {
                                    let idx_map_name = format!("idx_{}_{}", table_name.to_lowercase(), idx.name.to_lowercase());
                                    let has_null = new_vals.iter().any(|v| v.is_null());

                                    if idx.is_unique && !has_null && old_vals != new_vals {
                                        let existing = tx.scan_visible(&idx_map_name)?;
                                        for (k, _) in existing {
                                            if let Some((v, r_id)) = decode_composite_index_key(&k) {
                                                if v == new_vals && r_id != row_id {
                                                    return Err(H2Error::Execution(format!(
                                                        "Unique constraint violation on index '{}': duplicate value {:?}",
                                                        idx.name, new_vals
                                                    )));
                                                }
                                            }
                                        }
                                    }

                                    let old_key = encode_composite_index_key(&old_vals, row_id);
                                    tx.remove(&idx_map_name, &old_key)?;
                                    let new_key = encode_composite_index_key(&new_vals, row_id);
                                    tx.put(&idx_map_name, new_key, vec![])?;
                                }
                            }

                            tx.put(&map_name, key.clone(), row.to_bytes()?)?;
                            updated_keys.insert(key);
                            affected_rows += 1;
                            updated_rows.push(row);
                        }
                    } else {
                        let matches = if let Some(sel) = &selection {
                            match evaluate_expr_context(sel, &target_ctx, &row)? {
                                Value::Boolean(b) => b,
                                _ => false,
                            }
                        } else {
                            true
                        };

                        if matches {
                            let old_row = row.clone();

                            for assignment in &assignments {
                                let col_name = match &assignment.target {
                                    sqlparser::ast::AssignmentTarget::ColumnName(object_name) => object_name.to_string(),
                                    _ => return Err(H2Error::Execution("Unsupported assignment target".to_string())),
                                };
                                let col_idx = table_def.column_index(&col_name).ok_or_else(|| {
                                    H2Error::Execution(format!("Column '{}' not found in table '{}'", col_name, table_name))
                                })?;
                                let new_val = evaluate_expr_context(&assignment.value, &target_ctx, &row)?;
                                let casted = new_val.cast_to(&table_def.columns[col_idx].data_type)?;
                                row.values[col_idx] = casted;
                            }

                            // 外部キー連動チェック・処理（親行としての更新）
                            self.handle_foreign_keys_on_update(tx, &table_name, &old_row, &row)?;

                            // 外部キー整合性チェック（子行としての更新）
                            self.validate_foreign_keys_for_row(tx, &table_def, &row)?;

                            // インデックス更新
                            for idx in &indexes {
                                if let (Some(old_vals), Some(new_vals)) = (
                                    get_index_values(&table_def, idx, &old_row),
                                    get_index_values(&table_def, idx, &row),
                                ) {
                                    let idx_map_name = format!("idx_{}_{}", table_name.to_lowercase(), idx.name.to_lowercase());
                                    let has_null = new_vals.iter().any(|v| v.is_null());

                                    if idx.is_unique && !has_null && old_vals != new_vals {
                                        let existing = tx.scan_visible(&idx_map_name)?;
                                        for (k, _) in existing {
                                            if let Some((v, r_id)) = decode_composite_index_key(&k) {
                                                if v == new_vals && r_id != row_id {
                                                    return Err(H2Error::Execution(format!(
                                                        "Unique constraint violation on index '{}': duplicate value {:?}",
                                                        idx.name, new_vals
                                                    )));
                                                }
                                            }
                                        }
                                    }

                                    let old_key = encode_composite_index_key(&old_vals, row_id);
                                    tx.remove(&idx_map_name, &old_key)?;
                                    let new_key = encode_composite_index_key(&new_vals, row_id);
                                    tx.put(&idx_map_name, new_key, vec![])?;
                                }
                            }

                            tx.put(&map_name, key.clone(), row.to_bytes()?)?;
                            updated_keys.insert(key);
                            affected_rows += 1;
                            updated_rows.push(row);
                        }
                    }
                }

                if let Some(ref ret) = returning {
                    if !ret.is_empty() {
                        let (cols, r_rows) = project_returning(&table_def, &updated_rows, ret)?;
                        return Ok(ExecutionResult::Query { columns: cols, rows: r_rows });
                    }
                }

                Ok(ExecutionResult::Dml { affected_rows })
            }
            Statement::Drop {
                object_type,
                names,
                if_exists,
                cascade,
                ..
            } => {
                match object_type {
                    sqlparser::ast::ObjectType::Table => {
                        for name in names {
                            let table_name = name.to_string();
                            if self.catalog.get_table(&table_name).is_none() {
                                if if_exists {
                                    continue;
                                }
                                return Err(H2Error::Catalog(format!("Table '{}' not found", table_name)));
                            }
                            let referencing = self.catalog.get_tables_referencing(&table_name);
                            if !referencing.is_empty() {
                                let child_names: Vec<String> = referencing.iter().map(|(t, _)| t.name.clone()).collect();
                                return Err(H2Error::Execution(format!(
                                    "Cannot drop table '{}' because it is referenced by: {}",
                                    table_name, child_names.join(", ")
                                )));
                            }
                            let dropped_maps = self.catalog.drop_table(&table_name)?;
                            for map_name in dropped_maps {
                                self.store.remove_map(&map_name);
                            }
                        }
                    }
                    sqlparser::ast::ObjectType::Index => {
                        for name in names {
                            let index_name = name.to_string();
                            if self.catalog.get_index(&index_name).is_none() {
                                if if_exists {
                                    continue;
                                }
                                return Err(H2Error::Catalog(format!("Index '{}' not found", index_name)));
                            }
                            let map_name = self.catalog.drop_index(&index_name)?;
                            self.store.remove_map(&map_name);
                        }
                    }
                    sqlparser::ast::ObjectType::View => {
                        for name in names {
                            self.catalog.drop_view(&name.to_string(), if_exists)?;
                        }
                    }
                    sqlparser::ast::ObjectType::Schema => {
                        for name in names {
                            let schema_name = name.to_string();
                            let dropped_maps = self.catalog.drop_schema(&schema_name, if_exists, cascade)?;
                            for map_name in dropped_maps {
                                self.store.remove_map(&map_name);
                            }
                        }
                    }
                    _ => {
                        return Err(H2Error::Execution(format!(
                            "Unsupported DROP object type: {:?}",
                            object_type
                        )));
                    }
                }
                self.store.commit()?;
                Ok(ExecutionResult::Ddl)
            }
            Statement::Query(query) => self.execute_query(tx, *query),
            Statement::Merge {
                table,
                source,
                on,
                clauses,
                ..
            } => {
                let target_name = match &table {
                    TableFactor::Table { name, .. } => name.to_string(),
                    _ => return Err(H2Error::Execution("Complex target table in MERGE not supported".to_string())),
                };
                let target_alias = match &table {
                    TableFactor::Table { alias, .. } => alias.as_ref().map(|a| a.name.value.clone()),
                    _ => None,
                };
                let target_table_def = self.catalog.get_table(&target_name).ok_or_else(|| {
                    H2Error::Catalog(format!("Table '{}' not found in MERGE", target_name))
                })?;
                let target_map_name = target_table_def.map_name();
                let target_indexes = self.catalog.get_table_indexes(&target_name);

                // ソース行を取得
                let (source_table_def, source_rows, source_alias) = match &source {
                    TableFactor::Table { name, alias, .. } => {
                        let s_name = name.to_string();
                        let s_alias = alias.as_ref().map(|a| a.name.value.clone());
                        let s_def = self.catalog.get_table(&s_name).ok_or_else(|| {
                            H2Error::Catalog(format!("Table '{}' not found in MERGE source", s_name))
                        })?;
                        let s_map = s_def.map_name();
                        let entries = tx.scan_visible(&s_map)?;
                        let mut rows = Vec::with_capacity(entries.len());
                        for (_, b) in entries {
                            let mut r = Row::from_bytes(&b)?;
                            s_def.align_row(&mut r);
                            rows.push(r);
                        }
                        (s_def, rows, s_alias)
                    }
                    _ => return Err(H2Error::Execution("Only simple table factors supported as MERGE source".to_string())),
                };

                // ターゲットの既存全行を取得
                let target_entries = tx.scan_visible(&target_map_name)?;
                let mut target_rows: Vec<(u64, Row)> = Vec::new();
                for (k, b) in target_entries {
                    let r_id = if k.len() == 8 {
                        u64::from_le_bytes(k.as_slice().try_into().unwrap())
                    } else {
                        0
                    };
                    let mut r = Row::from_bytes(&b)?;
                    target_table_def.align_row(&mut r);
                    target_rows.push((r_id, r));
                }

                let mut affected_rows = 0;

                // ソースの各行に対してマッチング
                for s_row in source_rows {
                    let mut matched_target: Option<(usize, u64, Row)> = None;

                    for (t_idx, (t_row_id, t_row)) in target_rows.iter().enumerate() {
                        let mut ctx = RowContext::from_table_def(&target_table_def, target_alias.as_deref());
                        let base_idx = target_table_def.columns.len();
                        ctx.append_table(&source_table_def, source_alias.as_deref(), base_idx);

                        let mut comb = t_row.values.clone();
                        comb.extend(s_row.values.clone());
                        let comb_row = Row::new(comb);

                        if let Value::Boolean(true) = evaluate_expr_context(&on, &ctx, &comb_row)? {
                            matched_target = Some((t_idx, *t_row_id, t_row.clone()));
                            break;
                        }
                    }

                    if let Some((_, t_row_id, old_target_row)) = matched_target {
                        // MATCHED
                        for clause in &clauses {
                            if matches!(clause.clause_kind, sqlparser::ast::MergeClauseKind::Matched) {
                                let mut ctx = RowContext::from_table_def(&target_table_def, target_alias.as_deref());
                                let base_idx = target_table_def.columns.len();
                                ctx.append_table(&source_table_def, source_alias.as_deref(), base_idx);
                                let mut comb = old_target_row.values.clone();
                                comb.extend(s_row.values.clone());
                                let comb_row = Row::new(comb);

                                if let Some(ref pred) = clause.predicate {
                                    if !matches!(evaluate_expr_context(pred, &ctx, &comb_row)?, Value::Boolean(true)) {
                                        continue;
                                    }
                                }

                                match &clause.action {
                                    sqlparser::ast::MergeAction::Update { assignments } => {
                                        let mut updated_row = old_target_row.clone();
                                        for assignment in assignments {
                                            let col_name = match &assignment.target {
                                                sqlparser::ast::AssignmentTarget::ColumnName(object_name) => object_name.to_string(),
                                                _ => return Err(H2Error::Execution("Unsupported assignment target".to_string())),
                                            };
                                            let col_idx = target_table_def.column_index(&col_name).ok_or_else(|| {
                                                H2Error::Execution(format!("Column '{}' not found in table '{}'", col_name, target_name))
                                            })?;
                                            let new_val = evaluate_expr_context(&assignment.value, &ctx, &comb_row)?;
                                            let casted = new_val.cast_to(&target_table_def.columns[col_idx].data_type)?;
                                            updated_row.values[col_idx] = casted;
                                        }

                                        self.handle_foreign_keys_on_update(tx, &target_name, &old_target_row, &updated_row)?;
                                        self.validate_foreign_keys_for_row(tx, &target_table_def, &updated_row)?;

                                        for idx in &target_indexes {
                                            if let (Some(old_vals), Some(new_vals)) = (
                                                get_index_values(&target_table_def, idx, &old_target_row),
                                                get_index_values(&target_table_def, idx, &updated_row),
                                            ) {
                                                let idx_map = format!("idx_{}_{}", target_table_def.name.to_lowercase(), idx.name.to_lowercase());
                                                let old_key = encode_composite_index_key(&old_vals, t_row_id);
                                                tx.remove(&idx_map, &old_key)?;
                                                let new_key = encode_composite_index_key(&new_vals, t_row_id);
                                                tx.put(&idx_map, new_key, vec![])?;
                                            }
                                        }

                                        tx.put(&target_map_name, t_row_id.to_le_bytes().to_vec(), updated_row.to_bytes()?)?;
                                        affected_rows += 1;
                                        break;
                                    }
                                    sqlparser::ast::MergeAction::Delete => {
                                        self.handle_foreign_keys_on_delete(tx, &target_name, &old_target_row)?;
                                        for idx in &target_indexes {
                                            if let Some(vals) = get_index_values(&target_table_def, idx, &old_target_row) {
                                                let idx_map = format!("idx_{}_{}", target_table_def.name.to_lowercase(), idx.name.to_lowercase());
                                                let key = encode_composite_index_key(&vals, t_row_id);
                                                tx.remove(&idx_map, &key)?;
                                            }
                                        }
                                        tx.remove(&target_map_name, &t_row_id.to_le_bytes())?;
                                        affected_rows += 1;
                                        break;
                                    }
                                    _ => {}
                                }
                            }
                        }
                    } else {
                        // NOT MATCHED
                        for clause in &clauses {
                            if matches!(clause.clause_kind, sqlparser::ast::MergeClauseKind::NotMatched) {
                                let ctx = RowContext::from_table_def(&source_table_def, source_alias.as_deref());
                                if let Some(ref pred) = clause.predicate {
                                    if !matches!(evaluate_expr_context(pred, &ctx, &s_row)?, Value::Boolean(true)) {
                                        continue;
                                    }
                                }

                                if let sqlparser::ast::MergeAction::Insert(ref insert_action) = clause.action {
                                    let mut full_row_values = vec![Value::Null; target_table_def.columns.len()];
                                    let insert_exprs = match &insert_action.kind {
                                        sqlparser::ast::MergeInsertKind::Values(values) => match values.rows.first() {
                                            Some(row_exprs) => row_exprs,
                                            None => return Err(H2Error::Execution("MERGE INSERT requires VALUES clause".to_string())),
                                        },
                                        _ => return Err(H2Error::Execution("Unsupported MERGE INSERT kind".to_string())),
                                    };

                                    let target_indices: Vec<usize> = if insert_action.columns.is_empty() {
                                        (0..target_table_def.columns.len()).collect()
                                    } else {
                                        let mut indices = Vec::new();
                                        for col in &insert_action.columns {
                                            let col_idx = target_table_def.column_index(&col.value).ok_or_else(|| {
                                                H2Error::Catalog(format!("Column '{}' not found in table '{}'", col.value, target_name))
                                            })?;
                                            indices.push(col_idx);
                                        }
                                        indices
                                    };

                                    for (i, expr) in insert_exprs.iter().enumerate() {
                                        if i < target_indices.len() {
                                            let col_idx = target_indices[i];
                                            let val = evaluate_expr_context(expr, &ctx, &s_row)?;
                                            let casted = val.cast_to(&target_table_def.columns[col_idx].data_type)?;
                                            full_row_values[col_idx] = casted;
                                        }
                                    }

                                    let new_row_id = self.catalog.allocate_row_id(&target_name)?;
                                    let new_row = Row::new(full_row_values);
                                    self.validate_foreign_keys_for_row(tx, &target_table_def, &new_row)?;

                                    for idx in &target_indexes {
                                        if let Some(vals) = get_index_values(&target_table_def, idx, &new_row) {
                                            let idx_map = format!("idx_{}_{}", target_table_def.name.to_lowercase(), idx.name.to_lowercase());
                                            let key = encode_composite_index_key(&vals, new_row_id);
                                            tx.put(&idx_map, key, vec![])?;
                                        }
                                    }

                                    tx.put(&target_map_name, new_row_id.to_le_bytes().to_vec(), new_row.to_bytes()?)?;
                                    affected_rows += 1;
                                    break;
                                }
                            }
                        }
                    }
                }

                Ok(ExecutionResult::Dml { affected_rows })
            }
            Statement::Truncate { table_names, .. } => {
                for target in table_names {
                    let table_name = target.name.to_string();
                    let mut table_def = self.catalog.get_table(&table_name).ok_or_else(|| {
                        H2Error::Catalog(format!("Table '{}' not found", table_name))
                    })?;

                    let referencing = self.catalog.get_tables_referencing(&table_name);
                    for (child_def, fk) in referencing {
                        let child_map = format!("tbl_{}", child_def.name.to_lowercase());
                        if !tx.scan_visible(&child_map)?.is_empty() {
                            return Err(H2Error::Execution(format!(
                                "Cannot truncate table '{}' because it is referenced by table '{}' (foreign key on column '{}')",
                                table_name, child_def.name, fk.column
                            )));
                        }
                    }

                    let map_name = table_def.map_name();
                    // 高速オンラインTruncate: 1行ずつの削除ループを廃止しO(1)でツリーを一括クリア
                    self.store.clear_map(&map_name);

                    // 関連インデックスマップも一括クリア
                    let indexes = self.catalog.get_table_indexes(&table_name);
                    for idx in indexes {
                        let idx_map = format!("idx_{}_{}", table_name.to_lowercase(), idx.name.to_lowercase());
                        self.store.clear_map(&idx_map);
                    }

                    // next_row_id リセット
                    table_def.next_row_id = 1;
                    self.catalog.update_table(table_def)?;
                }
                self.store.commit()?;
                Ok(ExecutionResult::Ddl)
            }
            Statement::AlterTable { name, if_exists, operations, .. } => {
                let table_name = name.to_string();
                if self.catalog.get_table(&table_name).is_none() {
                    if if_exists {
                        return Ok(ExecutionResult::Ddl);
                    }
                    return Err(H2Error::Catalog(format!("Table '{}' not found", table_name)));
                }

                for op in operations {
                    match op {
                        sqlparser::ast::AlterTableOperation::RenameTable { table_name: new_table_name } => {
                            let new_name = new_table_name.to_string();
                            let old_map = format!("tbl_{}", table_name.to_lowercase());
                            let new_map = format!("tbl_{}", new_name.to_lowercase());

                            // 高速オンラインRename: 全行コピー・削除ループを廃止し、マップキーの差し替えのみでO(1)完了
                            self.store.rename_map(&old_map, &new_map)?;

                            let indexes = self.catalog.get_table_indexes(&table_name);
                            for idx in indexes {
                                let old_idx_map = format!("idx_{}_{}", table_name.to_lowercase(), idx.name.to_lowercase());
                                let new_idx_map = format!("idx_{}_{}", new_name.to_lowercase(), idx.name.to_lowercase());
                                let _ = self.store.rename_map(&old_idx_map, &new_idx_map);
                            }

                            self.catalog.rename_table(&table_name, &new_name)?;
                        }
                        sqlparser::ast::AlterTableOperation::AddColumn { column_def, .. } => {
                            let mut table_def = self.catalog.get_table(&table_name).unwrap();
                            let col_name = column_def.name.value.clone();
                            if table_def.column_index(&col_name).is_some() {
                                return Err(H2Error::Catalog(format!(
                                    "Column '{}' already exists in table '{}'",
                                    col_name, table_name
                                )));
                            }
                            let dt = convert_data_type(&column_def.data_type)?;
                            let mut is_nullable = true;
                            for opt in &column_def.options {
                                if matches!(opt.option, sqlparser::ast::ColumnOption::NotNull) {
                                    is_nullable = false;
                                }
                            }
                            let phys_idx = table_def.next_physical_index();
                            let mut new_col = ColumnDef::new(col_name, dt, is_nullable, false);
                            new_col.physical_index = Some(phys_idx);
                            table_def.columns.push(new_col);

                            // Instant DDL: テーブルの全行スキャン＆物理書き換えは不要！
                            // 既存データはそのまま保持され、行読み出し時に table_def.align_row(&mut row) で自動補完される。
                            self.catalog.update_table(table_def)?;
                        }
                        sqlparser::ast::AlterTableOperation::DropColumn { column_name, if_exists: col_if_exists, .. } => {
                            let mut table_def = self.catalog.get_table(&table_name).unwrap();
                            let col_name = column_name.value.clone();
                            let Some(col_idx) = table_def.column_index(&col_name) else {
                                if col_if_exists {
                                    continue;
                                }
                                return Err(H2Error::Catalog(format!(
                                    "Column '{}' not found in table '{}'",
                                    col_name, table_name
                                )));
                            };

                            // Instant DDL: 全行スキャン＆物理削除は行わない！
                            // カタログから該当列を削除し、行読み出し時に table_def.align_row(&mut row) で論理投影される。
                            table_def.columns.remove(col_idx);

                            self.catalog.update_table(table_def)?;
                        }
                        _ => {
                            return Err(H2Error::Execution(format!(
                                "Unsupported ALTER TABLE operation: {:?}",
                                op
                            )));
                        }
                    }
                }
                self.store.commit()?;
                Ok(ExecutionResult::Ddl)
            }
            Statement::Explain { statement, .. } => {
                let plan_str = self.explain_statement(tx, *statement)?;
                let row = Row::new(vec![Value::String(plan_str)]);
                Ok(ExecutionResult::Query {
                    columns: vec!["PLAN".to_string()],
                    rows: vec![row],
                })
            }
            Statement::ShowDatabases { .. } | Statement::ShowSchemas { .. } => {
                let mut schemas = self.catalog.get_schemas();
                schemas.sort();
                let rows: Vec<Row> = schemas
                    .into_iter()
                    .map(|s| Row::new(vec![Value::String(s)]))
                    .collect();
                Ok(ExecutionResult::Query {
                    columns: vec!["Database".to_string()],
                    rows,
                })
            }
            Statement::ShowTables { .. } => {
                let tables = self.catalog.all_tables();
                let rows: Vec<Row> = tables
                    .into_iter()
                    .map(|t| Row::new(vec![Value::String(t.name)]))
                    .collect();
                Ok(ExecutionResult::Query {
                    columns: vec!["Table".to_string()],
                    rows,
                })
            }
            Statement::ShowColumns { show_options, .. } => {
                let table_name = if let Some(ref in_opt) = show_options.show_in {
                    if let Some(ref parent) = in_opt.parent_name {
                        parent.to_string()
                    } else {
                        return Err(H2Error::Execution("Expected table name in SHOW COLUMNS FROM <table>".to_string()));
                    }
                } else {
                    return Err(H2Error::Execution("Expected table name in SHOW COLUMNS FROM <table>".to_string()));
                };

                let table_def = self.catalog.get_table(&table_name).ok_or_else(|| {
                    H2Error::Catalog(format!("Table '{}' not found", table_name))
                })?;

                let rows: Vec<Row> = table_def
                    .columns
                    .iter()
                    .map(|c| {
                        Row::new(vec![
                            Value::String(c.name.clone()),
                            Value::String(c.data_type.to_string()),
                            Value::String(if c.is_nullable { "YES".to_string() } else { "NO".to_string() }),
                            Value::String(if c.is_primary_key { "PRI".to_string() } else { "".to_string() }),
                        ])
                    })
                    .collect();

                Ok(ExecutionResult::Query {
                    columns: vec![
                        "Field".to_string(),
                        "Type".to_string(),
                        "Null".to_string(),
                        "Key".to_string(),
                    ],
                    rows,
                })
            }
            _ => Err(H2Error::Execution(format!("Unsupported statement: {:?}", stmt))),
        }
    }

    fn validate_foreign_keys_for_row(
        &self,
        tx: &Transaction,
        table_def: &TableDef,
        row: &Row,
    ) -> H2Result<()> {
        for fk in &table_def.foreign_keys {
            let col_idx = match table_def.column_index(&fk.column) {
                Some(i) => i,
                None => continue,
            };
            let val = match row.values.get(col_idx) {
                Some(v) => v,
                None => continue,
            };
            if val.is_null() {
                // SQL標準: NULL は親に一致しなくてもよい
                continue;
            }

            let parent_table_def = self.catalog.get_table(&fk.foreign_table).ok_or_else(|| {
                H2Error::Execution(format!("Referenced parent table '{}' not found", fk.foreign_table))
            })?;
            let parent_col_idx = parent_table_def.column_index(&fk.foreign_column).ok_or_else(|| {
                H2Error::Execution(format!(
                    "Referenced column '{}' not found in parent table '{}'",
                    fk.foreign_column, fk.foreign_table
                ))
            })?;

            let parent_map = format!("tbl_{}", fk.foreign_table.to_lowercase());
            let entries = tx.scan_visible(&parent_map)?;
            let mut exists = false;
            for (_k, v) in entries {
                let mut p_row = Row::from_bytes(&v)?;
                parent_table_def.align_row(&mut p_row);
                if p_row.values.get(parent_col_idx) == Some(val) {
                    exists = true;
                    break;
                }
            }

            if !exists {
                return Err(H2Error::Execution(format!(
                    "Foreign key constraint violation: value {:?} in column '{}' of table '{}' does not exist in parent table '{}.{}'",
                    val, fk.column, table_def.name, fk.foreign_table, fk.foreign_column
                )));
            }
        }
        Ok(())
    }

    fn handle_foreign_keys_on_delete(
        &self,
        tx: &Transaction,
        parent_table: &str,
        parent_row: &Row,
    ) -> H2Result<()> {
        let parent_table_def = match self.catalog.get_table(parent_table) {
            Some(t) => t,
            None => return Ok(()),
        };

        let referencing = self.catalog.get_tables_referencing(parent_table);
        for (child_table_def, fk) in referencing {
            let parent_col_idx = match parent_table_def.column_index(&fk.foreign_column) {
                Some(i) => i,
                None => continue,
            };
            let parent_val = match parent_row.values.get(parent_col_idx) {
                Some(v) => v,
                None => continue,
            };
            if parent_val.is_null() {
                continue;
            }

            let child_col_idx = match child_table_def.column_index(&fk.column) {
                Some(i) => i,
                None => continue,
            };

            let child_map_name = format!("tbl_{}", child_table_def.name.to_lowercase());
            let child_entries = tx.scan_visible(&child_map_name)?;
            let mut matching_child_rows = Vec::new();
            for (c_key, c_val_bytes) in child_entries {
                let mut c_row = Row::from_bytes(&c_val_bytes)?;
                child_table_def.align_row(&mut c_row);
                if c_row.values.get(child_col_idx) == Some(parent_val) {
                    matching_child_rows.push((c_key, c_row));
                }
            }

            if matching_child_rows.is_empty() {
                continue;
            }

            match fk.on_delete {
                ForeignKeyAction::Restrict | ForeignKeyAction::NoAction => {
                    return Err(H2Error::Execution(format!(
                        "Foreign key constraint violation: cannot delete from table '{}' because record is referenced by table '{}' (foreign key on column '{}')",
                        parent_table, child_table_def.name, fk.column
                    )));
                }
                ForeignKeyAction::Cascade => {
                    let child_indexes = self.catalog.get_table_indexes(&child_table_def.name);
                    for (c_key, c_row) in matching_child_rows {
                        let c_row_id = if c_key.len() == 8 {
                            u64::from_le_bytes(c_key.as_slice().try_into().unwrap())
                        } else {
                            0
                        };

                        // 再帰的に孫テーブル等の外部キー連動処理
                        self.handle_foreign_keys_on_delete(tx, &child_table_def.name, &c_row)?;

                        // インデックスから削除
                        for idx in &child_indexes {
                            if let Some(ci) = child_table_def.column_index(&idx.columns[0]) {
                                let col_val = &c_row.values[ci];
                                let idx_map = format!("idx_{}_{}", child_table_def.name.to_lowercase(), idx.name.to_lowercase());
                                let idx_key = encode_index_key(col_val, c_row_id);
                                tx.remove(&idx_map, &idx_key)?;
                            }
                        }

                        tx.remove(&child_map_name, &c_key)?;
                    }
                }
                ForeignKeyAction::SetNull => {
                    let child_indexes = self.catalog.get_table_indexes(&child_table_def.name);
                    for (c_key, mut c_row) in matching_child_rows {
                        let c_row_id = if c_key.len() == 8 {
                            u64::from_le_bytes(c_key.as_slice().try_into().unwrap())
                        } else {
                            0
                        };
                        let old_val = c_row.values[child_col_idx].clone();
                        c_row.values[child_col_idx] = Value::Null;

                        // インデックス更新
                        for idx in &child_indexes {
                            if let Some(ci) = child_table_def.column_index(&idx.columns[0]) {
                                if ci == child_col_idx {
                                    let idx_map = format!("idx_{}_{}", child_table_def.name.to_lowercase(), idx.name.to_lowercase());
                                    let old_idx_key = encode_index_key(&old_val, c_row_id);
                                    tx.remove(&idx_map, &old_idx_key)?;
                                    let new_idx_key = encode_index_key(&Value::Null, c_row_id);
                                    tx.put(&idx_map, new_idx_key, vec![])?;
                                }
                            }
                        }

                        tx.put(&child_map_name, c_key, c_row.to_bytes()?)?;
                    }
                }
            }
        }

        Ok(())
    }

    fn handle_foreign_keys_on_update(
        &self,
        tx: &Transaction,
        parent_table: &str,
        old_parent_row: &Row,
        new_parent_row: &Row,
    ) -> H2Result<()> {
        let parent_table_def = match self.catalog.get_table(parent_table) {
            Some(t) => t,
            None => return Ok(()),
        };

        let referencing = self.catalog.get_tables_referencing(parent_table);
        for (child_table_def, fk) in referencing {
            let parent_col_idx = match parent_table_def.column_index(&fk.foreign_column) {
                Some(i) => i,
                None => continue,
            };
            let old_val = match old_parent_row.values.get(parent_col_idx) {
                Some(v) => v,
                None => continue,
            };
            let new_val = match new_parent_row.values.get(parent_col_idx) {
                Some(v) => v,
                None => continue,
            };

            if old_val == new_val || old_val.is_null() {
                continue;
            }

            let child_col_idx = match child_table_def.column_index(&fk.column) {
                Some(i) => i,
                None => continue,
            };

            let child_map_name = format!("tbl_{}", child_table_def.name.to_lowercase());
            let child_entries = tx.scan_visible(&child_map_name)?;
            let mut matching_child_rows = Vec::new();
            for (c_key, c_val_bytes) in child_entries {
                let mut c_row = Row::from_bytes(&c_val_bytes)?;
                child_table_def.align_row(&mut c_row);
                if c_row.values.get(child_col_idx) == Some(old_val) {
                    matching_child_rows.push((c_key, c_row));
                }
            }

            if matching_child_rows.is_empty() {
                continue;
            }

            match fk.on_update {
                ForeignKeyAction::Restrict | ForeignKeyAction::NoAction => {
                    return Err(H2Error::Execution(format!(
                        "Foreign key constraint violation: cannot update referenced key in table '{}' because record is referenced by table '{}' (foreign key on column '{}')",
                        parent_table, child_table_def.name, fk.column
                    )));
                }
                ForeignKeyAction::Cascade => {
                    let child_indexes = self.catalog.get_table_indexes(&child_table_def.name);
                    for (c_key, mut c_row) in matching_child_rows {
                        let c_row_id = if c_key.len() == 8 {
                            u64::from_le_bytes(c_key.as_slice().try_into().unwrap())
                        } else {
                            0
                        };
                        c_row.values[child_col_idx] = new_val.clone();

                        // インデックス更新
                        for idx in &child_indexes {
                            if let Some(ci) = child_table_def.column_index(&idx.columns[0]) {
                                if ci == child_col_idx {
                                    let idx_map = format!("idx_{}_{}", child_table_def.name.to_lowercase(), idx.name.to_lowercase());
                                    let old_idx_key = encode_index_key(old_val, c_row_id);
                                    tx.remove(&idx_map, &old_idx_key)?;
                                    let new_idx_key = encode_index_key(new_val, c_row_id);
                                    tx.put(&idx_map, new_idx_key, vec![])?;
                                }
                            }
                        }

                        tx.put(&child_map_name, c_key, c_row.to_bytes()?)?;
                    }
                }
                ForeignKeyAction::SetNull => {
                    let child_indexes = self.catalog.get_table_indexes(&child_table_def.name);
                    for (c_key, mut c_row) in matching_child_rows {
                        let c_row_id = if c_key.len() == 8 {
                            u64::from_le_bytes(c_key.as_slice().try_into().unwrap())
                        } else {
                            0
                        };
                        c_row.values[child_col_idx] = Value::Null;

                        // インデックス更新
                        for idx in &child_indexes {
                            if let Some(ci) = child_table_def.column_index(&idx.columns[0]) {
                                if ci == child_col_idx {
                                    let idx_map = format!("idx_{}_{}", child_table_def.name.to_lowercase(), idx.name.to_lowercase());
                                    let old_idx_key = encode_index_key(old_val, c_row_id);
                                    tx.remove(&idx_map, &old_idx_key)?;
                                    let new_idx_key = encode_index_key(&Value::Null, c_row_id);
                                    tx.put(&idx_map, new_idx_key, vec![])?;
                                }
                            }
                        }

                        tx.put(&child_map_name, c_key, c_row.to_bytes()?)?;
                    }
                }
            }
        }

        Ok(())
    }

    fn explain_statement(&self, _tx: &Transaction, stmt: Statement) -> H2Result<String> {
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

                    let mut scan_type = format!("TableScan: {}", base_table_name);
                    if from_table.joins.is_empty() {
                        if let Some(Expr::BinaryOp { left, op: BinaryOperator::Eq, .. }) = &select.selection {
                            if let Expr::Identifier(ident) = left.as_ref() {
                                let col_name = &ident.value;
                                let indexes = self.catalog.get_table_indexes(&base_table_name);
                                if let Some(target_idx) = indexes.iter().find(|i| !i.name.starts_with("pk_") && i.columns[0].eq_ignore_ascii_case(col_name)) {
                                    scan_type = format!("IndexScan: {} on index {}", base_table_name, target_idx.name);
                                }
                            }
                        }
                    }
                    lines.push(scan_type);

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
            Statement::Update { table, .. } => Ok(format!("Update {}", table.relation)),
            Statement::Delete(_) => Ok("Delete".to_string()),
            _ => Ok(format!("Statement: {:?}", stmt)),
        }
    }

    #[allow(dead_code)]
    fn preprocess_subqueries(&self, tx: &Transaction, expr: &Expr) -> H2Result<Expr> {
        self.preprocess_subqueries_with_ctes(tx, expr, &HashMap::new())
    }

    fn preprocess_subqueries_with_ctes(
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

    fn execute_query(&self, tx: &Transaction, query: Query) -> H2Result<ExecutionResult> {
        self.execute_query_with_ctes(tx, query, &HashMap::new())
    }

    fn execute_query_with_ctes(
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
                    let ctx = RowContext::new();
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
                    TableFactor::Table { name, alias, .. } => {
                        let base_table_name = name.to_string();
                        let base_table_alias = alias.as_ref().map(|a| a.name.value.clone());

                        if let Some((cte_def, cte_rows)) = current_ctes.get(&base_table_name.to_lowercase()) {
                            let mut t_def = cte_def.clone();
                            if let Some(ref a) = base_table_alias {
                                t_def.name = a.clone();
                            }
                            (t_def, cte_rows.clone(), base_table_alias)
                        } else if let Some(view) = self.catalog.get_view(&base_table_name) {
                            self.resolve_view_query(tx, &view, base_table_alias, &current_ctes)?
                        } else {
                            let is_info_tables = base_table_name.eq_ignore_ascii_case("information_schema.tables")
                                || base_table_name.eq_ignore_ascii_case("tables");
                            let is_info_columns = base_table_name.eq_ignore_ascii_case("information_schema.columns")
                                || base_table_name.eq_ignore_ascii_case("columns");
                            let is_info_schemata = base_table_name.eq_ignore_ascii_case("information_schema.schemata")
                                || base_table_name.eq_ignore_ascii_case("schemata");

                            let (base_table_def, rows) = if is_info_tables {
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
                                    ],
                                );
                                let tables = self.catalog.all_tables();
                                let mut rows = Vec::new();
                                for t in tables {
                                    rows.push(Row::new(vec![
                                        Value::String(t.name.clone()),
                                        Value::Integer(t.columns.len() as i32),
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

                                let map_name = table_def.map_name();

                                // IndexScan の最適化
                                let mut index_scanned: Option<Vec<Row>> = None;
                                if from_table.joins.is_empty() {
                                    if let Some(Expr::BinaryOp { left, op: BinaryOperator::Eq, right }) = &select.selection {
                                        if let Expr::Identifier(ident) = left.as_ref() {
                                            let col_name = &ident.value;
                                            let indexes = self.catalog.get_table_indexes(&base_table_name);
                                            if let Some(target_idx) = indexes.iter().find(|i| i.columns[0].eq_ignore_ascii_case(col_name)) {
                                                if let Ok(search_val) = evaluate_literal_or_unary(right) {
                                                    let idx_map_name = format!("idx_{}_{}", table_def.name.to_lowercase(), target_idx.name.to_lowercase());
                                                    let idx_entries = tx.scan_visible(&idx_map_name)?;
                                                    let mut matched_row_ids = Vec::new();
                                                    for (k, _) in idx_entries {
                                                        if let Some((v, r_id)) = decode_index_key(&k) {
                                                            if v == search_val {
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
                    _ => return Err(H2Error::Execution("Complex table factors not supported".to_string())),
                };

                let mut current_rows = current_rows;
                let mut ctx = RowContext::from_table_def(&base_table_def, base_table_alias.as_deref());

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

                let final_rows = if let Some(lim) = limit_num {
                    final_rows.into_iter().skip(offset_num).take(lim).collect()
                } else {
                    final_rows.into_iter().skip(offset_num).collect()
                };

                Ok(ExecutionResult::Query {
                    columns: result_columns,
                    rows: final_rows,
                })
            }
            _ => Err(H2Error::Execution("Only SELECT queries or UNION are supported".to_string())),
        }
    }
}

fn has_aggregate_func(expr: &Expr) -> bool {
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

fn value_to_sql_expr(val: Value) -> Expr {
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

fn collect_window_functions(expr: &Expr, funcs: &mut Vec<sqlparser::ast::Function>) {
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

fn replace_window_function(expr: &Expr, window_func_str: &str, replacement: &Value) -> Expr {
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

fn get_func_arg_expr(arg: &sqlparser::ast::FunctionArg) -> H2Result<&Expr> {
    match arg {
        sqlparser::ast::FunctionArg::Unnamed(sqlparser::ast::FunctionArgExpr::Expr(e)) => Ok(e),
        _ => Err(H2Error::Execution("Unsupported function argument in window function".to_string())),
    }
}

fn compute_window_functions(
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



