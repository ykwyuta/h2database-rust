use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use sqlparser::ast::{
    Expr, SetExpr, Statement, TableFactor,
};
use h2_mvstore::Transaction;
use h2_types::{H2Error, H2Result, Value};
use h2_types::query_metrics::QueryMetricsGuard;
use crate::catalog::{ColumnDef, IndexDef};
use crate::expression::{
    evaluate_expr, evaluate_expr_context, evaluate_literal_or_unary, RowContext,
};
use crate::parser::{convert_data_type, extract_create_table};
use crate::row::Row;
use super::*;

impl SQLEngine {
    pub(crate) fn execute_statement(&self, tx: &Transaction, stmt: Statement) -> H2Result<ExecutionResult> {
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
            Statement::CreateSequence {
                name,
                if_not_exists,
                sequence_options,
                ..
            } => {
                let (schema, seq_name) = match name.0.len() {
                    2 => (name.0[0].value.clone(), name.0[1].value.clone()),
                    _ => ("public".to_string(), name.to_string()),
                };

                let mut increment_by = 1i64;
                let mut min_value = None;
                let mut max_value = None;
                let mut start_with = 1i64;
                let mut cycle = false;

                for opt in sequence_options {
                    match opt {
                        sqlparser::ast::SequenceOptions::IncrementBy(expr, _) => {
                            if let Ok(Value::Integer(v)) = evaluate_literal_or_unary(&expr) {
                                increment_by = v as i64;
                            } else if let Ok(Value::BigInt(v)) = evaluate_literal_or_unary(&expr) {
                                increment_by = v;
                            }
                        }
                        sqlparser::ast::SequenceOptions::MinValue(opt_expr) => {
                            min_value = opt_expr.and_then(|expr| {
                                evaluate_literal_or_unary(&expr).ok().and_then(|v| match v {
                                    Value::Integer(i) => Some(i as i64),
                                    Value::BigInt(i) => Some(i),
                                    _ => None,
                                })
                            });
                        }
                        sqlparser::ast::SequenceOptions::MaxValue(opt_expr) => {
                            max_value = opt_expr.and_then(|expr| {
                                evaluate_literal_or_unary(&expr).ok().and_then(|v| match v {
                                    Value::Integer(i) => Some(i as i64),
                                    Value::BigInt(i) => Some(i),
                                    _ => None,
                                })
                            });
                        }
                        sqlparser::ast::SequenceOptions::StartWith(expr, _) => {
                            if let Ok(Value::Integer(v)) = evaluate_literal_or_unary(&expr) {
                                start_with = v as i64;
                            } else if let Ok(Value::BigInt(v)) = evaluate_literal_or_unary(&expr) {
                                start_with = v;
                            }
                        }
                        sqlparser::ast::SequenceOptions::Cycle(b) => {
                            cycle = b;
                        }
                        _ => {}
                    }
                }

                let seq_def = crate::catalog::SequenceDef {
                    name: seq_name,
                    schema,
                    current_value: start_with,
                    increment_by,
                    min_value: min_value.unwrap_or(1),
                    max_value: max_value.unwrap_or(i64::MAX),
                    start_with,
                    cycle,
                    is_called: false,
                    owner_table: None,
                };

                self.catalog.create_sequence(seq_def, if_not_exists)?;
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

                for col in &table_def.columns {
                    if let Some(ref seq_name) = col.sequence_name {
                        let seq_def = crate::catalog::SequenceDef {
                            name: seq_name.clone(),
                            schema: table_def.schema.clone(),
                            current_value: 1,
                            increment_by: 1,
                            min_value: 1,
                            max_value: i64::MAX,
                            start_with: 1,
                            cycle: false,
                            is_called: false,
                            owner_table: Some(tbl_name.clone()),
                        };
                        let _ = self.catalog.create_sequence(seq_def, true);
                    }
                }

                if let Err(e) = self.catalog.create_table(table_def) {
                    if create_table.if_not_exists && e.to_string().contains("already exists") {
                        return Ok(ExecutionResult::Ddl);
                    }
                    return Err(e);
                }

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

                if table_def.is_queue {
                    return Err(H2Error::Unsupported(format!(
                        "Secondary indexes are not allowed on Queue Table '{}'",
                        table_name
                    )));
                }


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
                let table_name = normalize_object_name(&insert.table_name);
                let table_def = self.catalog.get_table(&table_name).ok_or_else(|| {
                    H2Error::Catalog(format!("Table '{}' not found", table_name))
                })?;

                let map_name = table_def.map_name();
                let indexes = self.catalog.get_table_indexes(&table_name);

                let target_indices: Vec<usize> = if insert.columns.is_empty() {
                    if table_def.is_queue {
                        let first_len = if let Some(ref source) = insert.source {
                            if matches!(*source.body, SetExpr::Values(ref v) if !v.rows.is_empty()) {
                                if let SetExpr::Values(ref v) = *source.body {
                                    v.rows[0].len()
                                } else {
                                    0
                                }
                            } else {
                                0
                            }
                        } else {
                            0
                        };
                        if first_len == table_def.columns.len().saturating_sub(4) {
                            (4..table_def.columns.len()).collect()
                        } else {
                            (0..table_def.columns.len()).collect()
                        }
                    } else if table_def.is_cache {
                        let first_len = if let Some(ref source) = insert.source {
                            if matches!(*source.body, SetExpr::Values(ref v) if !v.rows.is_empty()) {
                                if let SetExpr::Values(ref v) = *source.body {
                                    v.rows[0].len()
                                } else {
                                    0
                                }
                            } else {
                                0
                            }
                        } else {
                            0
                        };
                        if first_len == table_def.columns.len().saturating_sub(3) {
                            (3..table_def.columns.len()).collect()
                        } else {
                            (0..table_def.columns.len()).collect()
                        }
                    } else {
                        (0..table_def.columns.len()).collect()
                    }
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
                            let eval_ctx = RowContext {
                                columns: Vec::new(),
                                catalog: Some(Arc::clone(&self.catalog)),
                                dialect_mode: Some(self.dialect_mode()),
                            };
                            let dummy_row = Row::new(vec![]);
                            for row_exprs in values.rows {
                                let mut row_vals = Vec::with_capacity(row_exprs.len());
                                for expr in row_exprs {
                                    if let Expr::Identifier(ident) = &expr {
                                        if ident.value.eq_ignore_ascii_case("DEFAULT") {
                                            row_vals.push(Value::Null);
                                            continue;
                                        }
                                    }
                                    let val = evaluate_literal_or_unary(&expr)
                                        .or_else(|_| evaluate_expr_context(&expr, &eval_ctx, &dummy_row))?;
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
                        if !val.is_null() {
                            let casted = val.cast_to(&table_def.columns[col_idx].data_type)?;
                            full_row_values[col_idx] = casted;
                        }
                    }

                    // シーケンス列（SERIAL, BIGSERIAL, IDENTITY）の自動採番
                    for (col_idx, col_def) in table_def.columns.iter().enumerate() {
                        if full_row_values[col_idx].is_null() {
                            if let Some(ref seq_name) = col_def.sequence_name {
                                let next_v = self.catalog.nextval(seq_name)?;
                                full_row_values[col_idx] = Value::BigInt(next_v).cast_to(&col_def.data_type)?;
                            }
                        }
                    }

                    // キューテーブルのシステム列（_offset, _timestamp, _msg_id）の自動補完
                    let mut allocated_row_id = None;
                    if table_def.is_queue {
                        let r_id = self.catalog.allocate_row_id(&table_name)?;
                        if full_row_values[0].is_null() {
                            full_row_values[0] = Value::BigInt(r_id as i64);
                        }
                        if full_row_values[1].is_null() {
                            full_row_values[1] = Value::Timestamp(chrono::Utc::now());
                        }
                        if full_row_values[2].is_null() {
                            let offset_val = match &full_row_values[0] {
                                Value::BigInt(v) => *v,
                                _ => r_id as i64,
                            };
                            let uid = uuid::Uuid::new_v4().to_string();
                            full_row_values[2] = Value::String(format!("ID:h2-mq-{}-{}", offset_val, &uid[..8]));
                        }
                        allocated_row_id = Some(r_id);
                    }

                    if table_def.is_cache {
                        let now_ms = chrono::Utc::now().timestamp_millis();
                        if full_row_values[0].is_null() {
                            let ttl = table_def.cache_ttl_ms.unwrap_or(3600_000);
                            full_row_values[0] = Value::BigInt(now_ms + ttl as i64);
                        }
                        if full_row_values[1].is_null() {
                            full_row_values[1] = Value::BigInt(now_ms);
                        }
                        full_row_values[2] = Value::Boolean(true);
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
                                let prefix = encode_index_prefix(&vals);
                                let matched = tx.scan_prefix_visible(&idx_map_name, &prefix)?;
                                for (k, _) in matched {
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
                                                        let prefix = encode_index_prefix(&new_vals);
                                                        let matched = tx.scan_prefix_visible(&idx_map_name, &prefix)?;
                                                        for (k, _) in matched {
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
                    let row_id = if let Some(r_id) = allocated_row_id {
                        r_id
                    } else {
                        self.catalog.allocate_row_id(&table_name)?
                    };
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

                if table_def.is_queue && (table_def.retention_duration_ms.is_some() || table_def.max_bytes.is_some()) {
                    let _ = self.purge_queue_retention(tx, &table_name);
                }

                if let Some(ref returning) = insert.returning {
                    if !returning.is_empty() {
                        let (cols, res_rows) = project_returning(&table_def, &returning_rows, returning)?;
                        return Ok(ExecutionResult::Query { columns: cols, rows: res_rows });
                    }
                }

                let _ = self.catalog.update_approx_row_count(&table_name, affected_rows as i64);
                self.autovacuum.record_insert(&table_name, affected_rows);

                Ok(ExecutionResult::Dml { affected_rows })
            }
            Statement::Delete(delete) => {
                let from_table = match &delete.from {
                    sqlparser::ast::FromTable::WithFromKeyword(tables) => &tables[0],
                    sqlparser::ast::FromTable::WithoutKeyword(tables) => &tables[0],
                };
                let table_name = match &from_table.relation {
                    TableFactor::Table { name, .. } => normalize_object_name(name),
                    _ => return Err(H2Error::Execution("Complex table factors not supported".to_string())),
                };
                let target_alias = match &from_table.relation {
                    TableFactor::Table { alias, .. } => alias.as_ref().map(|a| a.name.value.clone()),
                    _ => None,
                };

                let table_def = self.catalog.get_table(&table_name).ok_or_else(|| {
                    H2Error::Catalog(format!("Table '{}' not found", table_name))
                })?;

                if table_def.is_queue {
                    return Err(H2Error::Unsupported(format!(
                        "DELETE is not allowed on Queue Table '{}'",
                        table_name
                    )));
                }


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
                let entries = if using_data.is_none() {
                    let mut fast_entries = None;
                    if let Some(ref sel) = delete.selection {
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
                                            let key_bytes = r_id.to_le_bytes().to_vec();
                                            if let Some(val_bytes) = tx.get(&map_name, &key_bytes)? {
                                                fetched.push((key_bytes, val_bytes));
                                            }
                                        }
                                    }
                                    fast_entries = Some(fetched);
                                    break;
                                }
                            }
                        }
                    }
                    if let Some(fe) = fast_entries {
                        fe
                    } else {
                        tx.scan_visible(&map_name)?
                    }
                } else {
                    tx.scan_visible(&map_name)?
                };
                let mut affected_rows = 0;
                let mut deleted_rows = Vec::new();
                let target_ctx = RowContext::from_table_def(&table_def, target_alias.as_deref());

                for (key, val_bytes) in entries {
                    let mut row = Row::from_bytes(&val_bytes)?;
                    table_def.align_row(&mut row);

                    if table_def.is_cache {
                        let now_ms = chrono::Utc::now().timestamp_millis();
                        if let Some(Value::BigInt(exp)) = row.values.get(0) {
                            if *exp <= now_ms {
                                continue;
                            }
                        }
                    }

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

                let _ = self.catalog.update_approx_row_count(&table_name, -(affected_rows as i64));
                self.autovacuum.record_delete(&table_name, affected_rows);

                Ok(ExecutionResult::Dml { affected_rows })
            }
            Statement::Update { table, assignments, from, selection, returning, .. } => {
                let table_name = match &table.relation {
                    TableFactor::Table { name, .. } => normalize_object_name(name),
                    _ => return Err(H2Error::Execution("Complex table factors in UPDATE not supported".to_string())),
                };
                let target_alias = match &table.relation {
                    TableFactor::Table { alias, .. } => alias.as_ref().map(|a| a.name.value.clone()),
                    _ => None,
                };

                let table_def = self.catalog.get_table(&table_name).ok_or_else(|| {
                    H2Error::Catalog(format!("Table '{}' not found", table_name))
                })?;

                if table_def.is_queue {
                    return Err(H2Error::Unsupported(format!(
                        "UPDATE is not allowed on Queue Table '{}'",
                        table_name
                    )));
                }


                let from_data: Option<(RowContext, Vec<Row>)> = if let Some(ref from_twj) = from {
                    Some(self.evaluate_table_with_joins(tx, from_twj, &HashMap::new())?)
                } else {
                    None
                };

                let map_name = table_def.map_name();
                let indexes = self.catalog.get_table_indexes(&table_name);
                let collect_returning = returning.as_ref().is_some_and(|items| !items.is_empty());
                let has_fk = !table_def.foreign_keys.is_empty();
                let has_referencing = !self.catalog.get_tables_referencing(&table_name).is_empty();

                let mut any_idx_col_modified = false;
                let mut parsed_assignments = Vec::with_capacity(assignments.len());
                for assignment in &assignments {
                    let col_name = match &assignment.target {
                        sqlparser::ast::AssignmentTarget::ColumnName(object_name) => object_name.to_string(),
                        _ => return Err(H2Error::Execution("Unsupported assignment target".to_string())),
                    };
                    let col_idx = table_def.column_index(&col_name).ok_or_else(|| {
                        H2Error::Execution(format!("Column '{}' not found in table '{}'", col_name, table_name))
                    })?;
                    if indexes.iter().any(|idx| idx.columns.iter().any(|c| c.eq_ignore_ascii_case(&col_name))) {
                        any_idx_col_modified = true;
                    }
                    parsed_assignments.push((col_name, col_idx, &assignment.value));
                }

                // ================= P1 Point Update Fast Path =================
                // 単一テーブル、主キー/一意等値条件、FROM なし、RETURNING なし、
                // 非インデックス列変更、外部キーなしの専用超高速更新経路
                if from_data.is_none() && !collect_returning && !any_idx_col_modified && !has_fk && !has_referencing {
                    if let Some(ref sel) = selection {
                        if let Some((col_name, val)) = Self::extract_equality_predicate(sel) {
                            if let Some(target_idx) = indexes.iter().find(|idx| idx.is_unique && idx.columns.len() == 1 && idx.columns[0].eq_ignore_ascii_case(&col_name)) {
                                let idx_map_name = format!("idx_{}_{}", table_def.name.to_lowercase(), target_idx.name.to_lowercase());
                                let casted_val = if let Some(c_idx) = table_def.column_index(&col_name) {
                                    val.cast_to(&table_def.columns[c_idx].data_type).unwrap_or(val.clone())
                                } else {
                                    val.clone()
                                };
                                let prefix = encode_index_prefix(std::slice::from_ref(&casted_val));
                                let matched_entries = tx.scan_prefix_visible(&idx_map_name, &prefix)?;
                                if let Some((k, _)) = matched_entries.first() {
                                    if let Some((_v, r_id)) = decode_index_key(k) {
                                        let key_bytes = r_id.to_le_bytes().to_vec();
                                        if let Some(val_bytes) = tx.get(&map_name, &key_bytes)? {
                                            let mut row = Row::from_bytes(&val_bytes)?;
                                            table_def.align_row(&mut row);

                                            if table_def.is_cache {
                                                let now_ms = chrono::Utc::now().timestamp_millis();
                                                if let Some(Value::BigInt(exp)) = row.values.get(0) {
                                                    if *exp <= now_ms {
                                                        return Ok(ExecutionResult::Dml { affected_rows: 0 });
                                                    }
                                                }
                                            }

                                            let target_ctx = RowContext::from_table_def(&table_def, target_alias.as_deref());
                                            for (_col_name, col_idx, val_expr) in &parsed_assignments {
                                                let new_val = evaluate_expr_context(val_expr, &target_ctx, &row)?;
                                                let casted = new_val.cast_to(&table_def.columns[*col_idx].data_type)?;
                                                row.values[*col_idx] = casted;
                                            }

                                            if table_def.is_cache && row.values.len() > 2 {
                                                row.values[2] = Value::Boolean(true);
                                            }

                                            tx.put(&map_name, key_bytes, row.to_bytes()?)?;
                                            return Ok(ExecutionResult::Dml { affected_rows: 1 });
                                        }
                                    }
                                }
                                return Ok(ExecutionResult::Dml { affected_rows: 0 });
                            }
                        }
                    }
                }

                let entries = if from_data.is_none() {
                    let mut fast_entries = None;
                    if let Some(ref sel) = selection {
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
                                            let key_bytes = r_id.to_le_bytes().to_vec();
                                            if let Some(val_bytes) = tx.get(&map_name, &key_bytes)? {
                                                fetched.push((key_bytes, val_bytes));
                                            }
                                        }
                                    }
                                    fast_entries = Some(fetched);
                                    break;
                                }
                            }
                        } else {
                            // Range Scan プッシュダウン
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
                                        let mut fetched = Vec::with_capacity(matched.len());
                                        for (k, _) in matched {
                                            if let Some((_v, r_id)) = decode_index_key(&k) {
                                                let key_bytes = r_id.to_le_bytes().to_vec();
                                                if let Some(val_bytes) = tx.get(&map_name, &key_bytes)? {
                                                    fetched.push((key_bytes, val_bytes));
                                                }
                                            }
                                        }
                                        fast_entries = Some(fetched);
                                        break;
                                    }
                                }
                            }
                        }
                    }
                    if let Some(fe) = fast_entries {
                        fe
                    } else {
                        tx.scan_visible(&map_name)?
                    }
                } else {
                    tx.scan_visible(&map_name)?
                };
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

                    if table_def.is_cache {
                        let now_ms = chrono::Utc::now().timestamp_millis();
                        if let Some(Value::BigInt(exp)) = row.values.get(0) {
                            if *exp <= now_ms {
                                continue;
                            }
                        }
                    }

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
                            let old_row = if has_referencing || any_idx_col_modified {
                                Some(row.clone())
                            } else {
                                None
                            };

                            for (_col_name, col_idx, val_expr) in &parsed_assignments {
                                let new_val = evaluate_expr_context(val_expr, &merged_ctx, &combined_row)?;
                                let casted = new_val.cast_to(&table_def.columns[*col_idx].data_type)?;
                                row.values[*col_idx] = casted;
                            }

                            if table_def.is_cache && row.values.len() > 2 {
                                row.values[2] = Value::Boolean(true);
                            }

                            if has_referencing {
                                self.handle_foreign_keys_on_update(tx, &table_name, old_row.as_ref().unwrap(), &row)?;
                            }
                            if has_fk {
                                self.validate_foreign_keys_for_row(tx, &table_def, &row)?;
                            }

                            if any_idx_col_modified {
                                let old_row_ref = old_row.as_ref().unwrap();
                                for idx in &indexes {
                                    if let (Some(old_vals), Some(new_vals)) = (
                                        get_index_values(&table_def, idx, old_row_ref),
                                        get_index_values(&table_def, idx, &row),
                                    ) {
                                        let idx_map_name = format!("idx_{}_{}", table_name.to_lowercase(), idx.name.to_lowercase());
                                        let has_null = new_vals.iter().any(|v| v.is_null());

                                        if idx.is_unique && !has_null && old_vals != new_vals {
                                            let prefix = encode_index_prefix(&new_vals);
                                            let matched = tx.scan_prefix_visible(&idx_map_name, &prefix)?;
                                            for (k, _) in matched {
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

                                        if old_vals != new_vals {
                                            let old_key = encode_composite_index_key(&old_vals, row_id);
                                            tx.remove(&idx_map_name, &old_key)?;
                                            let new_key = encode_composite_index_key(&new_vals, row_id);
                                            tx.put(&idx_map_name, new_key, vec![])?;
                                        }
                                    }
                                }
                            }

                            tx.put(&map_name, key.clone(), row.to_bytes()?)?;
                            updated_keys.insert(key);
                            affected_rows += 1;
                            if collect_returning {
                                updated_rows.push(row);
                            }
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
                            let old_row = if has_referencing || any_idx_col_modified {
                                Some(row.clone())
                            } else {
                                None
                            };

                            for (_col_name, col_idx, val_expr) in &parsed_assignments {
                                let new_val = evaluate_expr_context(val_expr, &target_ctx, &row)?;
                                let casted = new_val.cast_to(&table_def.columns[*col_idx].data_type)?;
                                row.values[*col_idx] = casted;
                            }

                            if table_def.is_cache && row.values.len() > 2 {
                                row.values[2] = Value::Boolean(true);
                            }

                            if has_referencing {
                                self.handle_foreign_keys_on_update(tx, &table_name, old_row.as_ref().unwrap(), &row)?;
                            }
                            if has_fk {
                                self.validate_foreign_keys_for_row(tx, &table_def, &row)?;
                            }

                            if any_idx_col_modified {
                                let old_row_ref = old_row.as_ref().unwrap();
                                for idx in &indexes {
                                    if let (Some(old_vals), Some(new_vals)) = (
                                        get_index_values(&table_def, idx, old_row_ref),
                                        get_index_values(&table_def, idx, &row),
                                    ) {
                                        let idx_map_name = format!("idx_{}_{}", table_name.to_lowercase(), idx.name.to_lowercase());
                                        let has_null = new_vals.iter().any(|v| v.is_null());

                                        if idx.is_unique && !has_null && old_vals != new_vals {
                                            let prefix = encode_index_prefix(&new_vals);
                                            let matched = tx.scan_prefix_visible(&idx_map_name, &prefix)?;
                                            for (k, _) in matched {
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

                                        if old_vals != new_vals {
                                            let old_key = encode_composite_index_key(&old_vals, row_id);
                                            tx.remove(&idx_map_name, &old_key)?;
                                            let new_key = encode_composite_index_key(&new_vals, row_id);
                                            tx.put(&idx_map_name, new_key, vec![])?;
                                        }
                                    }
                                }
                            }

                            tx.put(&map_name, key.clone(), row.to_bytes()?)?;
                            updated_keys.insert(key);
                            affected_rows += 1;
                            if collect_returning {
                                updated_rows.push(row);
                            }
                        }
                    }
                }

                if let Some(ref ret) = returning {
                    if !ret.is_empty() {
                        let (cols, r_rows) = project_returning(&table_def, &updated_rows, ret)?;
                        return Ok(ExecutionResult::Query { columns: cols, rows: r_rows });
                    }
                }

                self.autovacuum.record_update(&table_name, affected_rows);

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
                            let table_name = normalize_object_name(&name);
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
                    sqlparser::ast::ObjectType::Sequence => {
                        for name in names {
                            self.catalog.drop_sequence(&name.to_string(), if_exists)?;
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

                    if table_def.is_queue {
                        return Err(H2Error::Unsupported(format!(
                            "TRUNCATE is not allowed on Queue Table '{}'",
                            table_name
                        )));
                    }


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

                    // next_row_id リセット & 行数統計リセット
                    table_def.next_row_id = 1;
                    table_def.approx_row_count = 0;
                    table_def.stats = None;
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
            Statement::Explain { analyze, statement, format, options, .. } => {
                if format.is_some() || options.is_some() {
                    return Err(H2Error::Unsupported("Only text EXPLAIN and EXPLAIN ANALYZE are supported".to_string()));
                }
                let mut plan_str = self.explain_statement(tx, *statement.clone())?;
                if analyze {
                    let before = QueryMetricsGuard::snapshot();
                    let started = Instant::now();
                    let result = self.execute_statement(tx, *statement)?;
                    let elapsed = started.elapsed();
                    let counters = QueryMetricsGuard::snapshot().since(before);
                    let actual_rows = match result {
                        ExecutionResult::Query { rows, .. } => rows.len() as u64,
                        ExecutionResult::Dml { affected_rows } => affected_rows,
                        ExecutionResult::Ddl => 0,
                    };
                    plan_str.push_str(&format!(
                        "\nActual Rows: {}\nExecution Time: {:.3} ms\nWaits: row_lock={:.3} ms, tree_lock={:.3} ms, commit_lock={:.3} ms, wal_lock={:.3} ms, wal_write={:.3} ms, wal_sync={:.3} ms\nStorage: point_gets={}, scans={}, scan_entries={}\nTiming scope: statement execution; auto-commit is excluded",
                        actual_rows, elapsed.as_secs_f64() * 1000.0,
                        counters.lock_wait_ns as f64 / 1_000_000.0,
                        counters.tree_lock_wait_ns as f64 / 1_000_000.0,
                        counters.commit_lock_wait_ns as f64 / 1_000_000.0,
                        counters.wal_lock_wait_ns as f64 / 1_000_000.0,
                        counters.wal_write_ns as f64 / 1_000_000.0,
                        counters.wal_sync_ns as f64 / 1_000_000.0,
                        counters.point_gets, counters.scans, counters.scan_entries,
                    ));
                }
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
                        parent.to_string().trim_matches('"').to_string()
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
            Statement::Copy {
                source,
                to,
                target,
                options,
                legacy_options,
                ..
            } => {
                let file_path = match target {
                    sqlparser::ast::CopyTarget::File { filename } => filename,
                    _ => return Err(H2Error::Execution("Only COPY to/from file is supported".to_string())),
                };

                let mut delimiter = ',';
                let mut header = false;

                for opt in options {
                    match opt {
                        sqlparser::ast::CopyOption::Delimiter(c) => delimiter = c,
                        sqlparser::ast::CopyOption::Header(b) => header = b,
                        _ => {}
                    }
                }
                for opt in legacy_options {
                    match opt {
                        sqlparser::ast::CopyLegacyOption::Delimiter(c) => delimiter = c,
                        sqlparser::ast::CopyLegacyOption::Csv(csv_opts) => {
                            for c_opt in csv_opts {
                                if let sqlparser::ast::CopyLegacyCsvOption::Header = c_opt {
                                    header = true;
                                }
                            }
                        }
                        _ => {}
                    }
                }

                if to {
                    // COPY ... TO <file>
                    let (cols, rows) = match source {
                        sqlparser::ast::CopySource::Table { table_name, columns } => {
                            let tbl_name_str = table_name.to_string();
                            let t_def = self.catalog.get_table(&tbl_name_str).ok_or_else(|| {
                                H2Error::Catalog(format!("Table '{}' not found in COPY", tbl_name_str))
                            })?;
                            let map_name = t_def.map_name();
                            let entries = tx.scan_visible(&map_name)?;
                            let col_indices: Vec<usize> = if columns.is_empty() {
                                (0..t_def.columns.len()).collect()
                            } else {
                                let mut idxs = Vec::new();
                                for col in &columns {
                                    let c_idx = t_def.column_index(&col.value).ok_or_else(|| {
                                        H2Error::Catalog(format!("Column '{}' not found in table '{}'", col.value, tbl_name_str))
                                    })?;
                                    idxs.push(c_idx);
                                }
                                idxs
                            };
                            let mut rows = Vec::new();
                            for (_k, val_bytes) in entries {
                                let mut r = Row::from_bytes(&val_bytes)?;
                                t_def.align_row(&mut r);
                                let projected = col_indices.iter().map(|&i| r.get(i).cloned().unwrap_or(Value::Null)).collect();
                                rows.push(Row::new(projected));
                            }
                            let c_names = if columns.is_empty() {
                                t_def.columns.iter().map(|c| c.name.clone()).collect()
                            } else {
                                columns.into_iter().map(|c| c.value).collect()
                            };
                            (c_names, rows)
                        }
                        sqlparser::ast::CopySource::Query(query) => {
                            let exec_res = self.execute_query(tx, *query)?;
                            match exec_res {
                                ExecutionResult::Query { columns, rows } => (columns, rows),
                                _ => return Err(H2Error::Execution("COPY source query must return rows".to_string())),
                            }
                        }
                    };

                    let mut out = String::new();
                    if header {
                        out.push_str(&cols.join(&delimiter.to_string()));
                        out.push('\n');
                    }
                    for row in &rows {
                        let fields: Vec<String> = row.values.iter().map(|v| format_csv_field(v, delimiter)).collect();
                        out.push_str(&fields.join(&delimiter.to_string()));
                        out.push('\n');
                    }
                    std::fs::write(&file_path, out).map_err(|e| H2Error::Storage(e.to_string()))?;
                    Ok(ExecutionResult::Dml { affected_rows: rows.len() as u64 })
                } else {
                    // COPY ... FROM <file>
                    let (table_name, columns) = match source {
                        sqlparser::ast::CopySource::Table { table_name, columns } => (table_name.to_string(), columns),
                        _ => return Err(H2Error::Execution("COPY FROM requires table target".to_string())),
                    };
                    let table_def = self.catalog.get_table(&table_name).ok_or_else(|| {
                        H2Error::Catalog(format!("Table '{}' not found in COPY", table_name))
                    })?;
                    let target_indices: Vec<usize> = if columns.is_empty() {
                        (0..table_def.columns.len()).collect()
                    } else {
                        let mut idxs = Vec::new();
                        for col in &columns {
                            let c_idx = table_def.column_index(&col.value).ok_or_else(|| {
                                H2Error::Catalog(format!("Column '{}' not found in table '{}'", col.value, table_name))
                            })?;
                            idxs.push(c_idx);
                        }
                        idxs
                    };

                    let content = std::fs::read_to_string(&file_path).map_err(|e| H2Error::Storage(e.to_string()))?;
                    let mut lines = content.lines();
                    if header {
                        lines.next();
                    }

                    let mut affected_rows = 0;
                    for line in lines {
                        let trimmed = line.trim();
                        if trimmed.is_empty() { continue; }
                        let fields = parse_csv_line(trimmed, delimiter);
                        if fields.len() != target_indices.len() {
                            return Err(H2Error::Execution(format!(
                                "Column count mismatch in CSV import: expected {}, got {}",
                                target_indices.len(), fields.len()
                            )));
                        }

                        let mut full_row_values = vec![Value::Null; table_def.columns.len()];
                        for (i, field_str) in fields.into_iter().enumerate() {
                            let col_idx = target_indices[i];
                            let col_type = &table_def.columns[col_idx].data_type;
                            let val = Value::String(field_str).cast_to(col_type)?;
                            full_row_values[col_idx] = val;
                        }

                        for (col_idx, col_def) in table_def.columns.iter().enumerate() {
                            if full_row_values[col_idx].is_null() {
                                if let Some(ref seq_name) = col_def.sequence_name {
                                    let next_v = self.catalog.nextval(seq_name)?;
                                    full_row_values[col_idx] = Value::BigInt(next_v).cast_to(&col_def.data_type)?;
                                }
                            }
                        }

                        let row = Row::new(full_row_values);
                        let row_id = self.catalog.allocate_row_id(&table_name)?;
                        let row_bytes = row.to_bytes()?;
                        tx.put(&table_def.map_name(), row_id.to_le_bytes().to_vec(), row_bytes)?;

                        for idx in self.catalog.get_table_indexes(&table_name) {
                            if let Some(vals) = get_index_values(&table_def, &idx, &row) {
                                let idx_map_name = format!("idx_{}_{}", table_name.to_lowercase(), idx.name.to_lowercase());
                                let idx_key = encode_composite_index_key(&vals, row_id);
                                tx.put(&idx_map_name, idx_key, vec![])?;
                            }
                        }
                        affected_rows += 1;
                    }
                    self.store.commit()?;
                    Ok(ExecutionResult::Dml { affected_rows })
                }
            }
            Statement::Declare { stmts } => {
                for stmt in stmts {
                    let cursor_name = stmt.names.first().map(|n| n.value.to_lowercase()).unwrap_or_else(|| "cur".to_string());
                    let query = stmt.for_query.ok_or_else(|| {
                        H2Error::Execution("DECLARE cursor requires FOR <query> clause".to_string())
                    })?;
                    let scroll = stmt.scroll.unwrap_or(true);
                    let exec_res = self.execute_query(tx, *query)?;
                    let (cols, rows) = match exec_res {
                        ExecutionResult::Query { columns, rows } => (columns, rows),
                        _ => return Err(H2Error::Execution("Cursor query must return rows".to_string())),
                    };
                    let state = CursorState {
                        name: cursor_name.clone(),
                        columns: cols,
                        rows,
                        current_pos: -1,
                        scroll,
                    };
                    self.cursors.write().insert(cursor_name, state);
                }
                Ok(ExecutionResult::Ddl)
            }
            Statement::Fetch { name, direction, .. } => {
                let cursor_name = name.value.to_lowercase();
                let mut cursors = self.cursors.write();
                let cursor = cursors.get_mut(&cursor_name).ok_or_else(|| {
                    H2Error::Execution(format!("Cursor '{}' does not exist", name.value))
                })?;

                let total = cursor.rows.len() as isize;
                let fetched_rows: Vec<Row> = match direction {
                    sqlparser::ast::FetchDirection::Next => {
                        cursor.current_pos += 1;
                        if cursor.current_pos >= 0 && cursor.current_pos < total {
                            vec![cursor.rows[cursor.current_pos as usize].clone()]
                        } else {
                            vec![]
                        }
                    }
                    sqlparser::ast::FetchDirection::Prior => {
                        cursor.current_pos -= 1;
                        if cursor.current_pos >= 0 && cursor.current_pos < total {
                            vec![cursor.rows[cursor.current_pos as usize].clone()]
                        } else {
                            vec![]
                        }
                    }
                    sqlparser::ast::FetchDirection::First => {
                        cursor.current_pos = 0;
                        if total > 0 {
                            vec![cursor.rows[0].clone()]
                        } else {
                            vec![]
                        }
                    }
                    sqlparser::ast::FetchDirection::Last => {
                        cursor.current_pos = total - 1;
                        if total > 0 {
                            vec![cursor.rows[(total - 1) as usize].clone()]
                        } else {
                            vec![]
                        }
                    }
                    sqlparser::ast::FetchDirection::Absolute { limit } => {
                        let n: isize = match limit {
                            sqlparser::ast::Value::Number(s, _) => s.parse().unwrap_or(1),
                            _ => 1,
                        };
                        let target_idx = if n > 0 { n - 1 } else { total + n };
                        cursor.current_pos = target_idx;
                        if target_idx >= 0 && target_idx < total {
                            vec![cursor.rows[target_idx as usize].clone()]
                        } else {
                            vec![]
                        }
                    }
                    sqlparser::ast::FetchDirection::Relative { limit } => {
                        let n: isize = match limit {
                            sqlparser::ast::Value::Number(s, _) => s.parse().unwrap_or(1),
                            _ => 1,
                        };
                        cursor.current_pos += n;
                        if cursor.current_pos >= 0 && cursor.current_pos < total {
                            vec![cursor.rows[cursor.current_pos as usize].clone()]
                        } else {
                            vec![]
                        }
                    }
                    sqlparser::ast::FetchDirection::All | sqlparser::ast::FetchDirection::ForwardAll => {
                        let start = (cursor.current_pos + 1).max(0) as usize;
                        cursor.current_pos = total;
                        if start < cursor.rows.len() {
                            cursor.rows[start..].to_vec()
                        } else {
                            vec![]
                        }
                    }
                    sqlparser::ast::FetchDirection::Forward { limit } => {
                        let count: usize = match limit {
                            Some(sqlparser::ast::Value::Number(s, _)) => s.parse().unwrap_or(1),
                            _ => 1,
                        };
                        let mut result = Vec::new();
                        for _ in 0..count {
                            cursor.current_pos += 1;
                            if cursor.current_pos >= 0 && cursor.current_pos < total {
                                result.push(cursor.rows[cursor.current_pos as usize].clone());
                            } else {
                                break;
                            }
                        }
                        result
                    }
                    sqlparser::ast::FetchDirection::Count { limit } => {
                        let count: usize = match limit {
                            sqlparser::ast::Value::Number(s, _) => s.parse().unwrap_or(1),
                            _ => 1,
                        };
                        let mut result = Vec::new();
                        for _ in 0..count {
                            cursor.current_pos += 1;
                            if cursor.current_pos >= 0 && cursor.current_pos < total {
                                result.push(cursor.rows[cursor.current_pos as usize].clone());
                            } else {
                                break;
                            }
                        }
                        result
                    }
                    sqlparser::ast::FetchDirection::Backward { limit } => {
                        let count: usize = match limit {
                            Some(sqlparser::ast::Value::Number(s, _)) => s.parse().unwrap_or(1),
                            _ => 1,
                        };
                        let mut result = Vec::new();
                        for _ in 0..count {
                            cursor.current_pos -= 1;
                            if cursor.current_pos >= 0 && cursor.current_pos < total {
                                result.push(cursor.rows[cursor.current_pos as usize].clone());
                            } else {
                                break;
                            }
                        }
                        result
                    }
                    sqlparser::ast::FetchDirection::BackwardAll => {
                        let mut result = Vec::new();
                        while cursor.current_pos > 0 {
                            cursor.current_pos -= 1;
                            result.push(cursor.rows[cursor.current_pos as usize].clone());
                        }
                        cursor.current_pos = -1;
                        result
                    }
                };

                Ok(ExecutionResult::Query {
                    columns: cursor.columns.clone(),
                    rows: fetched_rows,
                })
            }
            Statement::Close { cursor } => {
                match cursor {
                    sqlparser::ast::CloseCursor::All => {
                        self.cursors.write().clear();
                    }
                    sqlparser::ast::CloseCursor::Specific { name } => {
                        let removed = self.cursors.write().remove(&name.value.to_lowercase());
                        if removed.is_none() {
                            return Err(H2Error::Execution(format!("Cursor '{}' does not exist", name.value)));
                        }
                    }
                }
                Ok(ExecutionResult::Ddl)
            }
            Statement::StartTransaction { .. }
            | Statement::Commit { .. }
            | Statement::Rollback { .. }
            | Statement::SetVariable { .. }
            | Statement::ShowVariable { .. }
            | Statement::ShowVariables { .. } => Ok(ExecutionResult::Ddl),
            _ => Err(H2Error::Execution(format!("Unsupported statement: {:?}", stmt))),
        }
    }

}
