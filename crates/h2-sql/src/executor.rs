use sqlparser::ast::{
    BinaryOperator, Expr, FunctionArg, FunctionArgExpr, GroupByExpr,
    JoinConstraint, JoinOperator, Query, SelectItem, SetExpr, Statement, TableFactor,
};


use std::sync::Arc;

use h2_mvstore::{MVStore, Transaction, TransactionStore};
use h2_types::{H2Error, H2Result, Value};
use crate::catalog::{Catalog, ColumnDef, IndexDef};
use crate::expression::{
    evaluate_expr, evaluate_expr_context, evaluate_literal_or_unary, ColumnBinding, RowContext,
};
use crate::parser::{convert_data_type, extract_create_table, parse_sql};
use crate::row::Row;

pub(crate) fn encode_index_key(val: &Value, row_id: u64) -> Vec<u8> {
    let mut bytes = serde_json::to_vec(val).unwrap_or_default();
    bytes.push(0x00);
    bytes.extend_from_slice(&row_id.to_be_bytes());
    bytes
}

pub(crate) fn decode_index_key(bytes: &[u8]) -> Option<(Value, u64)> {
    if bytes.len() < 9 {
        return None;
    }
    let val_bytes = &bytes[..bytes.len() - 9];
    let row_id_bytes = &bytes[bytes.len() - 8..];
    let row_id = u64::from_be_bytes(row_id_bytes.try_into().ok()?);
    let val = serde_json::from_slice(val_bytes).ok()?;
    Some((val, row_id))
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
                let tbl_map_name = format!("tbl_{}", table_name.to_lowercase());
                let entries = tx.scan_visible(&tbl_map_name)?;

                let first_col_idx = table_def.column_index(&col_names[0]).unwrap();
                let mut seen_keys = Vec::new();

                for (k, val_bytes) in entries {
                    let row = Row::from_bytes(&val_bytes)?;
                    let col_val = row.get(first_col_idx).cloned().unwrap_or(Value::Null);
                    let row_id = if k.len() == 8 {
                        u64::from_le_bytes(k.as_slice().try_into().unwrap())
                    } else {
                        0
                    };

                    if is_unique && !col_val.is_null() {
                        if seen_keys.contains(&col_val) {
                            return Err(H2Error::Execution(format!("Unique constraint violation on creating index '{}'", index_name)));
                        }
                        seen_keys.push(col_val.clone());
                    }


                    let idx_key = encode_index_key(&col_val, row_id);
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

                let map_name = format!("tbl_{}", table_name.to_lowercase());
                let indexes = self.catalog.get_table_indexes(&table_name);

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

                            // インデックス登録 & 一意性検証
                            for idx in &indexes {
                                if let Some(c_idx) = table_def.column_index(&idx.columns[0]) {
                                    let col_val = &row.values[c_idx];
                                    let idx_map_name = format!("idx_{}_{}", table_name.to_lowercase(), idx.name.to_lowercase());
                                    if idx.is_unique && !col_val.is_null() {
                                        let existing = tx.scan_visible(&idx_map_name)?;
                                        for (k, _) in existing {
                                            if let Some((v, _)) = decode_index_key(&k) {
                                                if &v == col_val {
                                                    return Err(H2Error::Execution(format!(
                                                        "Unique constraint violation on index '{}': duplicate value {:?}",
                                                        idx.name, col_val
                                                    )));
                                                }
                                            }
                                        }
                                    }
                                    let idx_key = encode_index_key(col_val, row_id);
                                    tx.put(&idx_map_name, idx_key, vec![])?;
                                }
                            }

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
                let indexes = self.catalog.get_table_indexes(&table_name);
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
                        let row_id = if key.len() == 8 {
                            u64::from_le_bytes(key.as_slice().try_into().unwrap())
                        } else {
                            0
                        };

                        // インデックスからキー削除
                        for idx in &indexes {
                            if let Some(c_idx) = table_def.column_index(&idx.columns[0]) {
                                let col_val = &row.values[c_idx];
                                let idx_map_name = format!("idx_{}_{}", table_name.to_lowercase(), idx.name.to_lowercase());
                                let idx_key = encode_index_key(col_val, row_id);
                                tx.remove(&idx_map_name, &idx_key)?;
                            }
                        }

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
                let indexes = self.catalog.get_table_indexes(&table_name);
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
                        let row_id = if key.len() == 8 {
                            u64::from_le_bytes(key.as_slice().try_into().unwrap())
                        } else {
                            0
                        };
                        let old_row = row.clone();

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

                        // インデックス更新
                        for idx in &indexes {
                            if let Some(c_idx) = table_def.column_index(&idx.columns[0]) {
                                let old_val = &old_row.values[c_idx];
                                let new_val = &row.values[c_idx];
                                let idx_map_name = format!("idx_{}_{}", table_name.to_lowercase(), idx.name.to_lowercase());

                                if idx.is_unique && !new_val.is_null() && old_val != new_val {
                                    let existing = tx.scan_visible(&idx_map_name)?;
                                    for (k, _) in existing {
                                        if let Some((v, r_id)) = decode_index_key(&k) {
                                            if &v == new_val && r_id != row_id {
                                                return Err(H2Error::Execution(format!(
                                                    "Unique constraint violation on index '{}': duplicate value {:?}",
                                                    idx.name, new_val
                                                )));
                                            }
                                        }
                                    }
                                }

                                let old_key = encode_index_key(old_val, row_id);
                                tx.remove(&idx_map_name, &old_key)?;
                                let new_key = encode_index_key(new_val, row_id);
                                tx.put(&idx_map_name, new_key, vec![])?;
                            }
                        }

                        tx.put(&map_name, key, row.to_bytes()?)?;
                        affected_rows += 1;
                    }
                }

                Ok(ExecutionResult::Dml { affected_rows })
            }
            Statement::Drop {
                object_type,
                names,
                if_exists,
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
            Statement::Truncate { table_names, .. } => {
                for target in table_names {
                    let table_name = target.name.to_string();
                    let mut table_def = self.catalog.get_table(&table_name).ok_or_else(|| {
                        H2Error::Catalog(format!("Table '{}' not found", table_name))
                    })?;

                    let map_name = format!("tbl_{}", table_name.to_lowercase());
                    let keys: Vec<Vec<u8>> = tx.scan_visible(&map_name)?
                        .into_iter()
                        .map(|(k, _)| k)
                        .collect();
                    for k in keys {
                        tx.remove(&map_name, &k)?;
                    }

                    // 関連インデックスマップも全件削除
                    let indexes = self.catalog.get_table_indexes(&table_name);
                    for idx in indexes {
                        let idx_map = format!("idx_{}_{}", table_name.to_lowercase(), idx.name.to_lowercase());
                        let idx_keys: Vec<Vec<u8>> = tx.scan_visible(&idx_map)?
                            .into_iter()
                            .map(|(k, _)| k)
                            .collect();
                        for k in idx_keys {
                            tx.remove(&idx_map, &k)?;
                        }
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

                            let rows = tx.scan_visible(&old_map)?;
                            for (k, v) in rows {
                                tx.put(&new_map, k.clone(), v)?;
                                tx.remove(&old_map, &k)?;
                            }
                            self.store.remove_map(&old_map);

                            let indexes = self.catalog.get_table_indexes(&table_name);
                            for idx in indexes {
                                let old_idx_map = format!("idx_{}_{}", table_name.to_lowercase(), idx.name.to_lowercase());
                                let new_idx_map = format!("idx_{}_{}", new_name.to_lowercase(), idx.name.to_lowercase());
                                let idx_entries = tx.scan_visible(&old_idx_map)?;
                                for (k, v) in idx_entries {
                                    tx.put(&new_idx_map, k.clone(), v)?;
                                    tx.remove(&old_idx_map, &k)?;
                                }
                                self.store.remove_map(&old_idx_map);
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
                            table_def.columns.push(ColumnDef {
                                name: col_name,
                                data_type: dt,
                                is_nullable,
                                is_primary_key: false,
                            });

                            let map_name = format!("tbl_{}", table_name.to_lowercase());
                            let entries = tx.scan_visible(&map_name)?;
                            for (k, v) in entries {
                                if let Ok(mut row) = Row::from_bytes(&v) {
                                    row.values.push(Value::Null);
                                    tx.put(&map_name, k, row.to_bytes()?)?;
                                }
                            }

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

                            table_def.columns.remove(col_idx);

                            let map_name = format!("tbl_{}", table_name.to_lowercase());
                            let entries = tx.scan_visible(&map_name)?;
                            for (k, v) in entries {
                                if let Ok(mut row) = Row::from_bytes(&v) {
                                    if col_idx < row.values.len() {
                                        row.values.remove(col_idx);
                                    }
                                    tx.put(&map_name, k, row.to_bytes()?)?;
                                }
                            }

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
                                if let Some(target_idx) = indexes.iter().find(|i| i.columns[0].eq_ignore_ascii_case(col_name)) {
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

        let is_info_tables = base_table_name.eq_ignore_ascii_case("information_schema.tables")
            || base_table_name.eq_ignore_ascii_case("tables");
        let is_info_columns = base_table_name.eq_ignore_ascii_case("information_schema.columns")
            || base_table_name.eq_ignore_ascii_case("columns");

        let (base_table_def, mut current_rows) = if is_info_tables {
            let t_def = crate::catalog::TableDef::new(
                base_table_name.clone(),
                vec![
                    crate::catalog::ColumnDef {
                        name: "table_name".to_string(),
                        data_type: h2_types::DataType::VarChar(None),
                        is_nullable: false,
                        is_primary_key: true,
                    },
                    crate::catalog::ColumnDef {
                        name: "columns_count".to_string(),
                        data_type: h2_types::DataType::Integer,
                        is_nullable: false,
                        is_primary_key: false,
                    },
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
                    crate::catalog::ColumnDef {
                        name: "table_name".to_string(),
                        data_type: h2_types::DataType::VarChar(None),
                        is_nullable: false,
                        is_primary_key: false,
                    },
                    crate::catalog::ColumnDef {
                        name: "column_name".to_string(),
                        data_type: h2_types::DataType::VarChar(None),
                        is_nullable: false,
                        is_primary_key: false,
                    },
                    crate::catalog::ColumnDef {
                        name: "data_type".to_string(),
                        data_type: h2_types::DataType::VarChar(None),
                        is_nullable: false,
                        is_primary_key: false,
                    },
                    crate::catalog::ColumnDef {
                        name: "is_nullable".to_string(),
                        data_type: h2_types::DataType::Boolean,
                        is_nullable: false,
                        is_primary_key: false,
                    },
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
        } else {
            let table_def = self.catalog.get_table(&base_table_name).ok_or_else(|| {
                H2Error::Catalog(format!("Table '{}' not found", base_table_name))
            })?;

            let map_name = format!("tbl_{}", base_table_name.to_lowercase());

            // IndexScan の最適化
            let mut index_scanned: Option<Vec<Row>> = None;
            if from_table.joins.is_empty() {
                if let Some(Expr::BinaryOp { left, op: BinaryOperator::Eq, right }) = &select.selection {
                    if let Expr::Identifier(ident) = left.as_ref() {
                        let col_name = &ident.value;
                        let indexes = self.catalog.get_table_indexes(&base_table_name);
                        if let Some(target_idx) = indexes.iter().find(|i| i.columns[0].eq_ignore_ascii_case(col_name)) {
                            if let Ok(search_val) = evaluate_literal_or_unary(right) {
                                let idx_map_name = format!("idx_{}_{}", base_table_name.to_lowercase(), target_idx.name.to_lowercase());
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
                                        fetched.push(Row::from_bytes(&val_bytes)?);
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
                    current_rows.push(Row::from_bytes(&val_bytes)?);
                }
                current_rows
            };
            (table_def, rows)
        };

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

