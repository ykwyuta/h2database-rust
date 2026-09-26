use sqlparser::ast::{
    BinaryOperator, Expr, FunctionArg, FunctionArgExpr, GroupByExpr,
    JoinConstraint, JoinOperator, Query, SelectItem, SetExpr, Statement, TableFactor,
};


use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Instant;
use chrono::Datelike;
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;

use h2_mvstore::{MVStore, Transaction, TransactionStore};
use h2_types::{H2Error, H2Result, Value};
use h2_types::query_metrics::QueryMetricsGuard;
use crate::catalog::{Catalog, ColumnDef, ForeignKeyAction, IndexDef, TableDef};
use crate::expression::{
    evaluate_expr, evaluate_expr_context, evaluate_literal_or_unary, ColumnBinding, RowContext,
};
use crate::parser::{convert_data_type, extract_create_table, parse_sql};
use crate::row::Row;

pub(crate) fn normalize_object_name(name: &sqlparser::ast::ObjectName) -> String {
    name.0.iter().map(|ident| ident.value.clone()).collect::<Vec<_>>().join(".")
}

pub(crate) fn encode_memcomparable_value(val: &Value, buf: &mut Vec<u8>) {
    match val {
        Value::Null => {
            buf.push(0x01);
        }
        Value::Boolean(b) => {
            buf.push(0x02);
            buf.push(if *b { 1 } else { 0 });
        }
        Value::TinyInt(i) => {
            buf.push(0x03);
            buf.push((*i as u8) ^ 0x80);
        }
        Value::SmallInt(i) => {
            buf.push(0x04);
            buf.extend_from_slice(&((*i as u16) ^ 0x8000).to_be_bytes());
        }
        Value::Integer(i) => {
            buf.push(0x05);
            buf.extend_from_slice(&((*i as u32) ^ 0x8000_0000).to_be_bytes());
        }
        Value::BigInt(i) => {
            buf.push(0x06);
            buf.extend_from_slice(&((*i as u64) ^ 0x8000_0000_0000_0000).to_be_bytes());
        }
        Value::Float(f) => {
            buf.push(0x07);
            let u = f.to_bits();
            let encoded = if (u & 0x8000_0000) != 0 {
                !u
            } else {
                u ^ 0x8000_0000
            };
            buf.extend_from_slice(&encoded.to_be_bytes());
        }
        Value::Double(f) => {
            buf.push(0x08);
            let u = f.to_bits();
            let encoded = if (u & 0x8000_0000_0000_0000) != 0 {
                !u
            } else {
                u ^ 0x8000_0000_0000_0000
            };
            buf.extend_from_slice(&encoded.to_be_bytes());
        }
        Value::String(s) => {
            buf.push(0x09);
            for &byte in s.as_bytes() {
                buf.push(byte);
                if byte == 0x00 {
                    buf.push(0xFF);
                }
            }
            buf.push(0x00);
            buf.push(0x00);
        }
        Value::Date(d) => {
            buf.push(0x0A);
            buf.extend_from_slice(&((d.num_days_from_ce() as u32) ^ 0x8000_0000).to_be_bytes());
        }
        Value::Timestamp(ts) => {
            buf.push(0x0B);
            let nanos = ts.timestamp_nanos_opt().unwrap_or(0);
            buf.extend_from_slice(&((nanos as u64) ^ 0x8000_0000_0000_0000).to_be_bytes());
        }
        Value::Decimal(dec) => {
            buf.push(0x0C);
            let f = dec.to_f64().unwrap_or(0.0);
            let u = f.to_bits();
            let encoded = if (u & 0x8000_0000_0000_0000) != 0 {
                !u
            } else {
                u ^ 0x8000_0000_0000_0000
            };
            buf.extend_from_slice(&encoded.to_be_bytes());
        }
        Value::Bytes(b) => {
            buf.push(0x0D);
            for &byte in b {
                buf.push(byte);
                if byte == 0x00 {
                    buf.push(0xFF);
                }
            }
            buf.push(0x00);
            buf.push(0x00);
        }
        Value::Uuid(u) => {
            buf.push(0x0E);
            buf.extend_from_slice(u.as_bytes());
        }
        _ => {
            buf.push(0xFF);
        }
    }
}

pub(crate) fn decode_memcomparable_value(bytes: &[u8]) -> Option<(Value, usize)> {
    if bytes.is_empty() {
        return None;
    }
    match bytes[0] {
        0x01 => Some((Value::Null, 1)),
        0x02 => {
            if bytes.len() < 2 { return None; }
            Some((Value::Boolean(bytes[1] != 0), 2))
        }
        0x03 => {
            if bytes.len() < 2 { return None; }
            let v = (bytes[1] ^ 0x80) as i8;
            Some((Value::TinyInt(v), 2))
        }
        0x04 => {
            if bytes.len() < 3 { return None; }
            let u = u16::from_be_bytes(bytes[1..3].try_into().ok()?);
            let v = (u ^ 0x8000) as i16;
            Some((Value::SmallInt(v), 3))
        }
        0x05 => {
            if bytes.len() < 5 { return None; }
            let u = u32::from_be_bytes(bytes[1..5].try_into().ok()?);
            let v = (u ^ 0x8000_0000) as i32;
            Some((Value::Integer(v), 5))
        }
        0x06 => {
            if bytes.len() < 9 { return None; }
            let u = u64::from_be_bytes(bytes[1..9].try_into().ok()?);
            let v = (u ^ 0x8000_0000_0000_0000) as i64;
            Some((Value::BigInt(v), 9))
        }
        0x07 => {
            if bytes.len() < 5 { return None; }
            let u = u32::from_be_bytes(bytes[1..5].try_into().ok()?);
            let decoded = if (u & 0x8000_0000) == 0 {
                !u
            } else {
                u ^ 0x8000_0000
            };
            Some((Value::Float(f32::from_bits(decoded)), 5))
        }
        0x08 => {
            if bytes.len() < 9 { return None; }
            let u = u64::from_be_bytes(bytes[1..9].try_into().ok()?);
            let decoded = if (u & 0x8000_0000_0000_0000) == 0 {
                !u
            } else {
                u ^ 0x8000_0000_0000_0000
            };
            Some((Value::Double(f64::from_bits(decoded)), 9))
        }
        0x09 => {
            let mut s_bytes = Vec::new();
            let mut i = 1;
            while i < bytes.len() {
                if bytes[i] == 0x00 {
                    if i + 1 < bytes.len() && bytes[i + 1] == 0xFF {
                        s_bytes.push(0x00);
                        i += 2;
                    } else if i + 1 < bytes.len() && bytes[i + 1] == 0x00 {
                        i += 2;
                        break;
                    } else {
                        i += 1;
                        break;
                    }
                } else {
                    s_bytes.push(bytes[i]);
                    i += 1;
                }
            }
            let s = String::from_utf8(s_bytes).ok()?;
            Some((Value::String(s), i))
        }
        0x0A => {
            if bytes.len() < 5 { return None; }
            let u = u32::from_be_bytes(bytes[1..5].try_into().ok()?);
            let num_days = (u ^ 0x8000_0000) as i32;
            let d = chrono::NaiveDate::from_num_days_from_ce_opt(num_days)?;
            Some((Value::Date(d), 5))
        }
        0x0B => {
            if bytes.len() < 9 { return None; }
            let u = u64::from_be_bytes(bytes[1..9].try_into().ok()?);
            let nanos = (u ^ 0x8000_0000_0000_0000) as i64;
            let dt = chrono::DateTime::from_timestamp_nanos(nanos);
            Some((Value::Timestamp(dt), 9))
        }
        0x0C => {
            if bytes.len() < 9 { return None; }
            let u = u64::from_be_bytes(bytes[1..9].try_into().ok()?);
            let decoded = if (u & 0x8000_0000_0000_0000) == 0 {
                !u
            } else {
                u ^ 0x8000_0000_0000_0000
            };
            let f = f64::from_bits(decoded);
            use rust_decimal::prelude::FromPrimitive;
            let dec = Decimal::from_f64(f).unwrap_or_default();
            Some((Value::Decimal(dec), 9))
        }
        0x0D => {
            let mut b_bytes = Vec::new();
            let mut i = 1;
            while i < bytes.len() {
                if bytes[i] == 0x00 {
                    if i + 1 < bytes.len() && bytes[i + 1] == 0xFF {
                        b_bytes.push(0x00);
                        i += 2;
                    } else if i + 1 < bytes.len() && bytes[i + 1] == 0x00 {
                        i += 2;
                        break;
                    } else {
                        i += 1;
                        break;
                    }
                } else {
                    b_bytes.push(bytes[i]);
                    i += 1;
                }
            }
            Some((Value::Bytes(b_bytes), i))
        }
        0x0E => {
            if bytes.len() < 17 { return None; }
            let u = uuid::Uuid::from_bytes(bytes[1..17].try_into().ok()?);
            Some((Value::Uuid(u), 17))
        }
        _ => None,
    }
}

pub(crate) fn decode_memcomparable_values(mut bytes: &[u8]) -> Option<Vec<Value>> {
    let mut vals = Vec::new();
    while !bytes.is_empty() {
        let (val, consumed) = decode_memcomparable_value(bytes)?;
        vals.push(val);
        bytes = &bytes[consumed..];
    }
    Some(vals)
}

pub(crate) fn encode_index_prefix(vals: &[Value]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(vals.len() * 9 + 1);
    for val in vals {
        encode_memcomparable_value(val, &mut bytes);
    }
    bytes.push(0x00);
    bytes
}

pub(crate) fn encode_composite_index_key(vals: &[Value], row_id: u64) -> Vec<u8> {
    let mut bytes = encode_index_prefix(vals);
    bytes.extend_from_slice(&row_id.to_be_bytes());
    bytes
}

pub(crate) fn encode_index_key(val: &Value, row_id: u64) -> Vec<u8> {
    encode_composite_index_key(std::slice::from_ref(val), row_id)
}

pub(crate) fn encode_index_key_max(vals: &[Value]) -> Vec<u8> {
    let mut bytes = encode_index_prefix(vals);
    bytes.extend_from_slice(&[0xFF; 8]);
    bytes
}

pub(crate) fn decode_composite_index_key(bytes: &[u8]) -> Option<(Vec<Value>, u64)> {
    if bytes.len() < 9 {
        return None;
    }
    let row_id_bytes = &bytes[bytes.len() - 8..];
    let row_id = u64::from_be_bytes(row_id_bytes.try_into().ok()?);
    let val_bytes = &bytes[..bytes.len() - 9];

    let vals = decode_memcomparable_values(val_bytes)?;
    Some((vals, row_id))
}

pub(crate) fn decode_index_key(bytes: &[u8]) -> Option<(Value, u64)> {
    if bytes.len() < 9 {
        return None;
    }
    let row_id_bytes = &bytes[bytes.len() - 8..];
    let row_id = u64::from_be_bytes(row_id_bytes.try_into().ok()?);
    let val_bytes = &bytes[..bytes.len() - 9];

    if let Some((v, _)) = decode_memcomparable_value(val_bytes) {
        return Some((v, row_id));
    }

    Some((Value::Null, row_id))
}

pub(crate) fn get_index_values(table_def: &TableDef, idx: &IndexDef, row: &Row) -> Option<Vec<Value>> {
    let mut vals = Vec::with_capacity(idx.columns.len());
    for col_name in &idx.columns {
        let c_idx = table_def.column_index(col_name)?;
        vals.push(row.values.get(c_idx).cloned().unwrap_or(Value::Null));
    }
    Some(vals)
}

pub(crate) fn execute_fast_row_aggregate(
    ops: &[crate::vectorized::VectorAggregateOp],
    filtered_rows: &[Row],
) -> Row {
    use rust_decimal::prelude::ToPrimitive;
    let count_star = filtered_rows.len() as i64;
    let mut sums = vec![0.0f64; ops.len()];
    let mut counts = vec![0i64; ops.len()];
    let mut mins: Vec<Option<Value>> = vec![None; ops.len()];
    let mut maxs: Vec<Option<Value>> = vec![None; ops.len()];
    let mut is_integral = vec![true; ops.len()];
    let mut int_sums = vec![0i64; ops.len()];

    for row in filtered_rows {
        for (op_idx, op) in ops.iter().enumerate() {
            match op {
                crate::vectorized::VectorAggregateOp::CountStar => {}
                crate::vectorized::VectorAggregateOp::Count(col_idx) => {
                    if let Some(v) = row.get(*col_idx) {
                        if !v.is_null() {
                            counts[op_idx] += 1;
                        }
                    }
                }
                crate::vectorized::VectorAggregateOp::Sum(col_idx) | crate::vectorized::VectorAggregateOp::Avg(col_idx) => {
                    if let Some(v) = row.get(*col_idx) {
                        match v {
                            Value::SmallInt(i) => {
                                int_sums[op_idx] = int_sums[op_idx].saturating_add(*i as i64);
                                sums[op_idx] += *i as f64;
                                counts[op_idx] += 1;
                            }
                            Value::Integer(i) => {
                                int_sums[op_idx] = int_sums[op_idx].saturating_add(*i as i64);
                                sums[op_idx] += *i as f64;
                                counts[op_idx] += 1;
                            }
                            Value::BigInt(i) => {
                                int_sums[op_idx] = int_sums[op_idx].saturating_add(*i);
                                sums[op_idx] += *i as f64;
                                counts[op_idx] += 1;
                            }
                            Value::Float(f) => {
                                is_integral[op_idx] = false;
                                sums[op_idx] += *f as f64;
                                counts[op_idx] += 1;
                            }
                            Value::Double(d) => {
                                is_integral[op_idx] = false;
                                sums[op_idx] += *d;
                                counts[op_idx] += 1;
                            }
                            Value::Decimal(dec) => {
                                is_integral[op_idx] = false;
                                if let Some(n) = dec.to_f64() {
                                    sums[op_idx] += n;
                                    counts[op_idx] += 1;
                                }
                            }
                            _ => {}
                        }
                    }
                }
                crate::vectorized::VectorAggregateOp::Min(col_idx) => {
                    if let Some(v) = row.get(*col_idx) {
                        if !v.is_null() {
                            mins[op_idx] = Some(match mins[op_idx].take() {
                                Some(curr) => if v < &curr { v.clone() } else { curr },
                                None => v.clone(),
                            });
                        }
                    }
                }
                crate::vectorized::VectorAggregateOp::Max(col_idx) => {
                    if let Some(v) = row.get(*col_idx) {
                        if !v.is_null() {
                            maxs[op_idx] = Some(match maxs[op_idx].take() {
                                Some(curr) => if v > &curr { v.clone() } else { curr },
                                None => v.clone(),
                            });
                        }
                    }
                }
            }
        }
    }

    let mut res_values = Vec::with_capacity(ops.len());
    for (op_idx, op) in ops.iter().enumerate() {
        let val = match op {
            crate::vectorized::VectorAggregateOp::CountStar => Value::BigInt(count_star),
            crate::vectorized::VectorAggregateOp::Count(_) => Value::BigInt(counts[op_idx]),
            crate::vectorized::VectorAggregateOp::Sum(_) => {
                if counts[op_idx] == 0 {
                    Value::Null
                } else if is_integral[op_idx] {
                    Value::BigInt(int_sums[op_idx])
                } else {
                    Value::Double(sums[op_idx])
                }
            }
            crate::vectorized::VectorAggregateOp::Avg(_) => {
                if counts[op_idx] == 0 {
                    Value::Null
                } else {
                    Value::Double(sums[op_idx] / counts[op_idx] as f64)
                }
            }
            crate::vectorized::VectorAggregateOp::Min(_) => match mins[op_idx].take() {
                Some(v) => v,
                None => Value::Null,
            },
            crate::vectorized::VectorAggregateOp::Max(_) => match maxs[op_idx].take() {
                Some(v) => v,
                None => Value::Null,
            },
        };
        res_values.push(val);
    }
    Row::new(res_values)
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

#[derive(Debug, Clone)]
pub struct CursorState {
    pub name: String,
    pub columns: Vec<String>,
    pub rows: Vec<Row>,
    pub current_pos: isize,
    pub scroll: bool,
}

pub struct SQLEngine {
    store: Arc<MVStore>,
    tx_store: Arc<TransactionStore>,
    catalog: Arc<Catalog>,
    cursors: Arc<parking_lot::RwLock<HashMap<String, CursorState>>>,
    read_only: Arc<std::sync::atomic::AtomicBool>,
    auth: Arc<crate::auth::AuthManager>,
    memory_config: crate::memory::MemoryConfig,
    admission: Arc<crate::memory::MemoryGrantCoordinator>,
    execution_mode: Arc<parking_lot::RwLock<String>>,
    plan_cache: Arc<parking_lot::RwLock<HashMap<String, Vec<Statement>>>>,
    query_stats: Arc<crate::query_stats::QueryStats>,
    procedural_engine: Arc<crate::procedural::ProceduralEngine>,
    dialect_mode: Arc<parking_lot::RwLock<h2_types::SqlDialectMode>>,
}

impl SQLEngine {
    pub fn new(store: Arc<MVStore>) -> H2Result<Self> {
        let tx_store = TransactionStore::new(Arc::clone(&store));
        let catalog = Arc::new(Catalog::new(Arc::clone(&store))?);
        let cursors = Arc::new(parking_lot::RwLock::new(HashMap::new()));
        let read_only = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let auth = Arc::new(crate::auth::AuthManager::new());
        let memory_config = crate::memory::MemoryConfig::new();
        let admission = crate::memory::MemoryGrantCoordinator::new(memory_config.total_query_memory());
        let execution_mode = Arc::new(parking_lot::RwLock::new("auto".to_string()));
        let plan_cache = Arc::new(parking_lot::RwLock::new(HashMap::new()));
        let procedural_engine = Arc::new(crate::procedural::ProceduralEngine::new());
        let dialect_mode = Arc::new(parking_lot::RwLock::new(h2_types::SqlDialectMode::Regular));
        Ok(Self {
            store,
            tx_store,
            catalog,
            cursors,
            read_only,
            auth,
            memory_config,
            admission,
            execution_mode,
            plan_cache,
            query_stats: Arc::new(crate::query_stats::QueryStats::default()),
            procedural_engine,
            dialect_mode,
        })
    }

    pub fn dialect_mode(&self) -> h2_types::SqlDialectMode {
        *self.dialect_mode.read()
    }

    pub fn set_dialect_mode(&self, mode: h2_types::SqlDialectMode) {
        *self.dialect_mode.write() = mode;
        self.plan_cache.write().clear();
    }

    pub fn procedural_engine(&self) -> &Arc<crate::procedural::ProceduralEngine> {
        &self.procedural_engine
    }

    pub fn execution_mode(&self) -> String {
        self.execution_mode.read().clone()
    }

    pub fn set_execution_mode(&self, mode: &str) {
        *self.execution_mode.write() = mode.to_lowercase();
    }

    pub fn memory_config(&self) -> &crate::memory::MemoryConfig {
        &self.memory_config
    }

    pub fn admission(&self) -> &Arc<crate::memory::MemoryGrantCoordinator> {
        &self.admission
    }

    pub fn set_max_materialized_rows(&self, rows: usize) {
        self.memory_config.set_max_materialized_rows(rows);
    }

    pub fn set_work_mem(&self, bytes: usize) {
        self.memory_config.set_work_mem(bytes);
    }

    pub fn auth(&self) -> &Arc<crate::auth::AuthManager> {
        &self.auth
    }

    pub fn set_read_only(&self, ro: bool) {
        self.read_only.store(ro, std::sync::atomic::Ordering::SeqCst);
    }

    pub fn is_read_only(&self) -> bool {
        self.read_only.load(std::sync::atomic::Ordering::SeqCst)
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
        self.execute_with_user(sql, None)
    }

    /// 暗黙トランザクション（Auto-commit）でユーザー指定でSQLを実行
    pub fn execute_with_user(&self, sql: &str, user: Option<&str>) -> H2Result<ExecutionResult> {
        let started = Instant::now();
        let metrics = QueryMetricsGuard::start();
        let trimmed = sql.trim().trim_end_matches(';').trim();
        let result = if trimmed.eq_ignore_ascii_case("VACUUM") {
            self.store.compact().map(|_| ExecutionResult::Ddl)
        } else {
            let tx = self.tx_store.begin();
            let result = self.execute_with_user_and_tx_inner(&tx, sql, user);
            match result {
                Ok(value) => tx.commit().map(|_| value),
                Err(error) => {
                    let _ = tx.rollback();
                    Err(error)
                }
            }
        };
        self.record_query(sql, started, metrics, &result);
        result
    }

    /// 明示的トランザクションコンテキストでSQLを実行 (デフォルトユーザー)
    pub fn execute_with_tx(&self, tx: &Transaction, sql: &str) -> H2Result<ExecutionResult> {
        self.execute_with_user_and_tx(tx, sql, None)
    }

    /// Commit an explicit transaction and attribute WAL waits to COMMIT.
    pub fn commit_transaction(&self, tx: &Transaction) -> H2Result<()> {
        let started = Instant::now();
        let metrics = QueryMetricsGuard::start();
        let result = tx.commit();
        self.query_stats.record("COMMIT", started.elapsed(), 0, result.is_err(), metrics.finish());
        result
    }

    /// ユーザー指定および明示的トランザクションコンテキストでSQLを実行
    pub fn execute_with_user_and_tx(&self, tx: &Transaction, sql: &str, user: Option<&str>) -> H2Result<ExecutionResult> {
        let started = Instant::now();
        let metrics = QueryMetricsGuard::start();
        let result = self.execute_with_user_and_tx_inner(tx, sql, user);
        self.record_query(sql, started, metrics, &result);
        result
    }

    fn record_query(&self, sql: &str, started: Instant, metrics: QueryMetricsGuard, result: &H2Result<ExecutionResult>) {
        let counters = metrics.finish();
        let command = sql.trim().trim_end_matches(';').trim();
        if command.eq_ignore_ascii_case("SHOW QUERY STATS") || command.eq_ignore_ascii_case("RESET QUERY STATS") {
            return;
        }
        let rows = match result {
            Ok(ExecutionResult::Query { rows, .. }) => rows.len() as u64,
            Ok(ExecutionResult::Dml { affected_rows }) => *affected_rows,
            _ => 0,
        };
        self.query_stats.record(sql, started.elapsed(), rows, result.is_err(), counters);
    }

    fn execute_with_user_and_tx_inner(&self, tx: &Transaction, sql: &str, user: Option<&str>) -> H2Result<ExecutionResult> {
        let trimmed = sql.trim().trim_end_matches(';').trim();
        let trimmed_upper = trimmed.to_uppercase();

        if trimmed_upper == "SHOW QUERY STATS" || trimmed_upper == "RESET QUERY STATS" {
            self.require_stats_admin(user)?;
            if trimmed_upper == "RESET QUERY STATS" {
                self.query_stats.reset();
                return Ok(ExecutionResult::Ddl);
            }
            return Ok(self.show_query_stats());
        }

        if self.is_read_only() {
            if trimmed.eq_ignore_ascii_case("VACUUM")
                || trimmed_upper.starts_with("RESTORE FROM ")
                || trimmed_upper.starts_with("RUNSCRIPT FROM ")
                || trimmed_upper.starts_with("ALTER SEQUENCE")
                || trimmed_upper.starts_with("CREATE USER")
                || trimmed_upper.starts_with("ALTER USER")
                || trimmed_upper.starts_with("DROP USER")
                || trimmed_upper.starts_with("GRANT ")
                || trimmed_upper.starts_with("REVOKE ")
            {
                return Err(H2Error::ReadOnly(
                    "Cannot execute data-modifying or DDL statements on a read-only instance".to_string(),
                ));
            }
        }

        if trimmed.eq_ignore_ascii_case("VACUUM") {
            self.store.compact()?;
            return Ok(ExecutionResult::Ddl);
        }

        // ================= DCL コマンド (ユーザー・権限管理) =================
        if trimmed_upper.starts_with("CREATE USER ") {
            return self.execute_create_user(trimmed);
        }
        if trimmed_upper.starts_with("ALTER USER ") {
            return self.execute_alter_user(trimmed);
        }
        if trimmed_upper.starts_with("DROP USER ") {
            return self.execute_drop_user(trimmed);
        }
        if trimmed_upper.starts_with("GRANT ") {
            return self.execute_grant(trimmed);
        }
        if trimmed_upper.starts_with("REVOKE ") {
            return self.execute_revoke(trimmed);
        }
        if trimmed_upper == "SHOW USERS" {
            return self.execute_show_users();
        }
        if trimmed_upper.starts_with("SHOW GRANTS FOR ") {
            let u = trimmed["SHOW GRANTS FOR ".len()..].trim().trim_matches('\'').trim_matches('"');
            return self.execute_show_grants(u);
        }

        if trimmed_upper.starts_with("RESTORE VERIFYONLY FROM ") {
            let path = extract_file_path(&trimmed["RESTORE VERIFYONLY FROM ".len()..]);
            let meta = self.verify_backup(&path)?;
            return Ok(ExecutionResult::Query {
                columns: vec![
                    "backup_id".to_string(),
                    "snapshot_version".to_string(),
                    "total_maps".to_string(),
                    "total_records".to_string(),
                    "total_bytes".to_string(),
                    "is_replica".to_string(),
                    "status".to_string(),
                ],
                rows: vec![crate::row::Row::new(vec![
                    Value::String(meta.backup_id),
                    Value::BigInt(meta.snapshot_version as i64),
                    Value::BigInt(meta.total_maps as i64),
                    Value::BigInt(meta.total_records as i64),
                    Value::BigInt(meta.total_bytes as i64),
                    Value::Boolean(meta.is_replica),
                    Value::String("VERIFIED".to_string()),
                ])],
            });
        }
        if trimmed_upper.starts_with("BACKUP TO ") {
            let path = extract_file_path(&trimmed[10..]);
            self.backup_to(&path)?;
            return Ok(ExecutionResult::Ddl);
        }
        if trimmed_upper.starts_with("RESTORE FROM ") {
            let rest = trimmed[13..].trim().trim_end_matches(';').trim();
            if let Some(with_idx) = rest.to_uppercase().find(" WITH ") {
                let path = extract_file_path(&rest[..with_idx]);
                let opts_str = &rest[with_idx + 6..];
                let (wal_archive, target) = parse_restore_pitr_options(opts_str)?;
                self.restore_pitr(&path, wal_archive.as_deref(), &target)?;
            } else {
                let path = extract_file_path(rest);
                self.restore_from(&path)?;
            }
            return Ok(ExecutionResult::Ddl);
        }
        if trimmed_upper.starts_with("SCRIPT TO ") {
            let path = extract_file_path(&trimmed[10..]);
            self.script_to(tx, &path)?;
            return Ok(ExecutionResult::Ddl);
        }
        if trimmed_upper.starts_with("RUNSCRIPT FROM ") {
            let path = extract_file_path(&trimmed[15..]);
            return self.runscript_from(&path);
        }

        if trimmed_upper.starts_with("ALTER SEQUENCE") {
            return self.execute_alter_sequence(trimmed);
        }

        if trimmed_upper.starts_with("CREATE QUEUE TABLE") {
            return self.execute_create_queue_table(tx, trimmed);
        }

        // ================= 統計情報 & メモリ管理コマンド =================
        if trimmed_upper == "ANALYZE" || trimmed_upper.starts_with("ANALYZE ") {
            return self.execute_analyze(tx, trimmed);
        }
        if trimmed_upper.starts_with("SET MAX_MATERIALIZED_ROWS") {
            return self.execute_set_max_materialized_rows(trimmed);
        }
        if trimmed_upper.starts_with("SET WORK_MEM") {
            return self.execute_set_work_mem(trimmed);
        }
        if trimmed_upper.starts_with("SET EXECUTION_MODE") {
            return self.execute_set_execution_mode(trimmed);
        }
        if trimmed_upper == "SHOW MAX_MATERIALIZED_ROWS" {
            let row = Row::new(vec![Value::Integer(self.memory_config.max_materialized_rows() as i32)]);
            return Ok(ExecutionResult::Query {
                columns: vec!["max_materialized_rows".to_string()],
                rows: vec![row],
            });
        }
        if trimmed_upper == "SHOW WORK_MEM" {
            let row = Row::new(vec![Value::BigInt(self.memory_config.work_mem() as i64)]);
            return Ok(ExecutionResult::Query {
                columns: vec!["work_mem".to_string()],
                rows: vec![row],
            });
        }
        if trimmed_upper == "SHOW EXECUTION_MODE" {
            let row = Row::new(vec![Value::String(self.execution_mode())]);
            return Ok(ExecutionResult::Query {
                columns: vec!["execution_mode".to_string()],
                rows: vec![row],
            });
        }

        // ================= PL/pgSQL プロシージャシミュレーション層 =================
        if trimmed_upper.starts_with("CREATE DATABASE ") || trimmed_upper.starts_with("DROP DATABASE ") {
            return Ok(ExecutionResult::Ddl);
        }
        if trimmed_upper.starts_with("EXEC DBMS_OUTPUT") || trimmed_upper.starts_with("EXECUTE DBMS_OUTPUT") {
            return Ok(ExecutionResult::Ddl);
        }
        if trimmed_upper.starts_with("CREATE OR REPLACE PROCEDURE ")
            || trimmed_upper.starts_with("CREATE PROCEDURE ")
            || trimmed_upper.starts_with("CREATE OR REPLACE FUNCTION ")
            || trimmed_upper.starts_with("CREATE FUNCTION ")
        {
            self.procedural_engine.register_from_ddl(&self.catalog, trimmed)?;
            return Ok(ExecutionResult::Ddl);
        }
        if trimmed_upper.starts_with("DROP PROCEDURE ") || trimmed_upper.starts_with("DROP FUNCTION ") {
            self.procedural_engine.drop_routine(&self.catalog, trimmed)?;
            return Ok(ExecutionResult::Ddl);
        }
        if trimmed_upper.starts_with("CALL ") {
            return self.procedural_engine.execute_call(&self.catalog, self, tx, user, trimmed);
        }
        if trimmed_upper.starts_with("DO ") || trimmed_upper.starts_with("DO$$") {
            let body_str = crate::procedural::plpgsql::parser::extract_body_source(trimmed)?;
            let mut lexer = crate::procedural::plpgsql::Lexer::new(&body_str);
            let tokens = lexer.tokenize()?;
            let mut parser = crate::procedural::plpgsql::PlPgSqlParser::new(tokens);
            let block = parser.parse_block()?;
            let mut interp = crate::procedural::ProcInterpreter::new(
                Some(Arc::clone(&self.catalog)),
                Some(self),
                Some(tx),
                user.map(|s| s.to_string()),
            );
            let mut env = crate::procedural::interpreter::ProcEnv::new();
            interp.execute_block(&block, &mut env)?;
            return Ok(ExecutionResult::Ddl);
        }



        if trimmed_upper == "SHOW MODE" {
            let mode = self.dialect_mode();
            return Ok(ExecutionResult::Query {
                columns: vec!["MODE".to_string()],
                rows: vec![crate::row::Row::new(vec![Value::String(mode.as_str().to_string())])],
            });
        }
        if trimmed_upper.starts_with("SET MODE ") {
            let mode_str = trimmed["SET MODE ".len()..].trim();
            if let Some(mode) = h2_types::SqlDialectMode::parse_str(mode_str) {
                self.set_dialect_mode(mode);
                return Ok(ExecutionResult::Ddl);
            } else {
                return Err(H2Error::Execution(format!("Unsupported SQL dialect mode: '{}'", mode_str)));
            }
        }

        let mut sql_to_parse = trimmed.to_string();
        if trimmed_upper.starts_with("FETCH RELATIVE -") {
            let num_part = trimmed["FETCH RELATIVE -".len()..].trim();
            sql_to_parse = format!("FETCH BACKWARD {}", num_part);
        }

        let mode = self.dialect_mode();
        let cache_key = format!("{}:{}", mode.as_str(), sql_to_parse);
        let statements = {
            let cached = self.plan_cache.read().get(&cache_key).cloned();
            if let Some(stmts) = cached {
                stmts
            } else {
                let parsed = crate::parser::parse_sql_mode(&sql_to_parse, mode)?;
                let mut cache = self.plan_cache.write();
                if cache.len() >= 1024 {
                    cache.clear();
                }
                cache.insert(cache_key, parsed.clone());
                parsed
            }
        };
        let mut last_result = ExecutionResult::Ddl;

        for stmt in statements {
            let subject = match &stmt {
                Statement::Explain { statement, .. } => statement.as_ref(),
                _ => &stmt,
            };
            let will_execute = !matches!(&stmt, Statement::Explain { analyze: false, .. });
            if self.is_read_only() && will_execute && Self::is_write_statement(subject) {
                return Err(H2Error::ReadOnly(
                    "Cannot execute data-modifying or DDL statements on a read-only instance".to_string(),
                ));
            }
            if let Some(username) = user {
                self.check_statement_privileges(username, subject)?;
            }
            last_result = self.execute_statement(tx, stmt)?;
        }

        Ok(last_result)
    }

    fn require_stats_admin(&self, user: Option<&str>) -> H2Result<()> {
        if !self.auth.is_auth_enabled() || user.is_none() {
            return Ok(());
        }
        let allowed = user.and_then(|name| {
            self.auth.list_users().into_iter().find(|u| u.username.eq_ignore_ascii_case(name))
        }).is_some_and(|u| u.is_superuser);
        if allowed {
            Ok(())
        } else {
            Err(H2Error::PermissionDenied("Query statistics require a superuser".to_string()))
        }
    }

    fn show_query_stats(&self) -> ExecutionResult {
        let columns = [
            "query", "calls", "errors", "rows", "total_ms", "mean_ms", "min_ms", "max_ms",
            "lock_wait_ms", "tree_lock_wait_ms", "commit_lock_wait_ms", "wal_lock_wait_ms", "wal_write_ms", "wal_sync_ms", "wal_durable_wait_ms",
            "point_gets", "scans", "scan_entries",
        ].into_iter().map(str::to_string).collect();
        let rows = self.query_stats.snapshot().into_iter().map(|s| {
            let ms = |ns: u64| ns as f64 / 1_000_000.0;
            Row::new(vec![
                Value::String(s.query),
                Value::BigInt(s.calls as i64),
                Value::BigInt(s.errors as i64),
                Value::BigInt(s.rows as i64),
                Value::Double(ms(s.total_ns)),
                Value::Double(ms(s.total_ns) / s.calls as f64),
                Value::Double(ms(s.min_ns)),
                Value::Double(ms(s.max_ns)),
                Value::Double(ms(s.counters.lock_wait_ns)),
                Value::Double(ms(s.counters.tree_lock_wait_ns)),
                Value::Double(ms(s.counters.commit_lock_wait_ns)),
                Value::Double(ms(s.counters.wal_lock_wait_ns)),
                Value::Double(ms(s.counters.wal_write_ns)),
                Value::Double(ms(s.counters.wal_sync_ns)),
                Value::Double(ms(s.counters.wal_durable_wait_ns)),
                Value::BigInt(s.counters.point_gets as i64),
                Value::BigInt(s.counters.scans as i64),
                Value::BigInt(s.counters.scan_entries as i64),
            ])
        }).collect();
        ExecutionResult::Query { columns, rows }
    }

    fn is_write_statement(stmt: &Statement) -> bool {
        match stmt {
            Statement::Query(_)
            | Statement::Explain { .. }
            | Statement::ShowTables { .. }
            | Statement::ShowColumns { .. }
            | Statement::ShowVariable { .. }
            | Statement::ShowVariables { .. }
            | Statement::Declare { .. }
            | Statement::Fetch { .. }
            | Statement::Close { .. } => false,
            Statement::Copy { to, .. } => !*to, // COPY ... FROM は書き込み
            _ => true,
        }
    }

    pub fn backup_to(&self, path: &str) -> H2Result<h2_mvstore::BackupMetadata> {
        self.store.dump_backup(path)
    }

    pub fn verify_backup(&self, path: &str) -> H2Result<h2_mvstore::BackupMetadata> {
        self.store.verify_backup(path)
    }

    pub fn restore_from(&self, path: &str) -> H2Result<()> {
        self.store.restore_backup(path)?;
        self.catalog.reload()?;
        Ok(())
    }

    pub fn restore_pitr<P: AsRef<std::path::Path>, A: AsRef<std::path::Path>>(
        &self,
        backup_path: P,
        archive_dir: Option<A>,
        target: &h2_mvstore::RecoveryTarget,
    ) -> H2Result<h2_mvstore::RestoreReport> {
        let report = self.store.restore_pitr(backup_path, archive_dir, target)?;
        self.catalog.reload()?;
        Ok(report)
    }

    pub fn script_to(&self, tx: &Transaction, path: &str) -> H2Result<()> {
        let mut sql = String::new();

        // 1. スキーマ
        for schema in self.catalog.get_schemas() {
            if schema != "public" {
                sql.push_str(&format!("CREATE SCHEMA IF NOT EXISTS \"{}\";\n", schema));
            }
        }

        // 2. シーケンス
        for seq in self.catalog.all_sequences() {
            if seq.owner_table.is_none() {
                sql.push_str(&format!(
                    "CREATE SEQUENCE IF NOT EXISTS \"{}\" INCREMENT BY {} START WITH {};\n",
                    seq.name, seq.increment_by, seq.current_value
                ));
            }
        }

        // 3. テーブル & データ
        for tbl in self.catalog.all_tables() {
            let mut col_strs = Vec::new();
            for col in &tbl.columns {
                let mut c_str = format!("\"{}\" {}", col.name, col.data_type);
                if !col.is_nullable {
                    c_str.push_str(" NOT NULL");
                }
                if col.is_primary_key && tbl.primary_key.len() == 1 {
                    c_str.push_str(" PRIMARY KEY");
                }
                col_strs.push(c_str);
            }
            if tbl.primary_key.len() > 1 {
                let pk_cols = tbl.primary_key.iter().map(|p| format!("\"{}\"", p)).collect::<Vec<_>>().join(", ");
                col_strs.push(format!("PRIMARY KEY ({})", pk_cols));
            }
            for u in &tbl.unique_constraints {
                let u_cols = u.columns.iter().map(|c| format!("\"{}\"", c)).collect::<Vec<_>>().join(", ");
                col_strs.push(format!("UNIQUE ({})", u_cols));
            }
            sql.push_str(&format!("CREATE TABLE IF NOT EXISTS {} ({});\n", tbl.name, col_strs.join(", ")));

            // データ INSERT
            let map_name = tbl.map_name();
            let entries = tx.scan_visible(&map_name)?;
            for (_k, val_bytes) in entries {
                let mut row = Row::from_bytes(&val_bytes)?;
                tbl.align_row(&mut row);
                let val_strs: Vec<String> = row.values.iter().map(|v| v.to_sql_literal()).collect();
                sql.push_str(&format!("INSERT INTO {} VALUES ({});\n", tbl.name, val_strs.join(", ")));
            }
        }

        // 4. 二次インデックス
        for idx in self.catalog.all_indexes() {
            if !idx.name.starts_with("pk_") && !idx.name.starts_with("uniq_") {
                let cols = idx.columns.iter().map(|c| format!("\"{}\"", c)).collect::<Vec<_>>().join(", ");
                let unique_kw = if idx.is_unique { "UNIQUE " } else { "" };
                sql.push_str(&format!("CREATE {}INDEX IF NOT EXISTS \"{}\" ON \"{}\" ({});\n", unique_kw, idx.name, idx.table_name, cols));
            }
        }

        // 5. ビュー
        for view in self.catalog.all_views() {
            sql.push_str(&format!("CREATE VIEW IF NOT EXISTS \"{}\" AS {};\n", view.name, view.query_sql));
        }

        std::fs::write(path, sql).map_err(|e| H2Error::Storage(e.to_string()))?;
        Ok(())
    }

    pub fn runscript_from(&self, path: &str) -> H2Result<ExecutionResult> {
        let content = std::fs::read_to_string(path).map_err(|e| H2Error::Storage(e.to_string()))?;
        let statements = split_sql_script(&content);
        let mut last_res = ExecutionResult::Ddl;
        for stmt in statements {
            last_res = self.execute(&stmt)?;
        }
        Ok(last_res)
    }

    fn execute_alter_sequence(&self, sql: &str) -> H2Result<ExecutionResult> {
        let tokens: Vec<&str> = sql.split_whitespace().collect();
        if tokens.len() < 3 {
            return Err(H2Error::SqlParse("Invalid ALTER SEQUENCE statement".to_string()));
        }
        let mut idx = 2;
        let mut if_exists = false;
        if tokens[idx].eq_ignore_ascii_case("IF") && idx + 1 < tokens.len() && tokens[idx + 1].eq_ignore_ascii_case("EXISTS") {
            if_exists = true;
            idx += 2;
        }
        if idx >= tokens.len() {
            return Err(H2Error::SqlParse("Missing sequence name in ALTER SEQUENCE".to_string()));
        }
        let seq_name = tokens[idx].trim_matches(|c| c == '"' || c == '\'' || c == '`');
        idx += 1;

        let mut restart_val: Option<i64> = None;
        let mut increment_by: Option<i64> = None;
        let mut min_val: Option<i64> = None;
        let mut max_val: Option<i64> = None;
        let mut cycle: Option<bool> = None;

        while idx < tokens.len() {
            let tok = tokens[idx].to_uppercase();
            if tok == "RESTART" {
                idx += 1;
                if idx < tokens.len() && tokens[idx].eq_ignore_ascii_case("WITH") {
                    idx += 1;
                }
                if idx < tokens.len() {
                    if let Ok(v) = tokens[idx].parse::<i64>() {
                        restart_val = Some(v);
                        idx += 1;
                    } else {
                        restart_val = Some(1);
                    }
                } else {
                    restart_val = Some(1);
                }
            } else if tok == "INCREMENT" {
                idx += 1;
                if idx < tokens.len() && tokens[idx].eq_ignore_ascii_case("BY") {
                    idx += 1;
                }
                if idx < tokens.len() {
                    let v = tokens[idx].parse::<i64>().map_err(|e| H2Error::SqlParse(e.to_string()))?;
                    increment_by = Some(v);
                    idx += 1;
                }
            } else if tok == "MINVALUE" {
                idx += 1;
                if idx < tokens.len() {
                    let v = tokens[idx].parse::<i64>().map_err(|e| H2Error::SqlParse(e.to_string()))?;
                    min_val = Some(v);
                    idx += 1;
                }
            } else if tok == "NO" && idx + 1 < tokens.len() && tokens[idx + 1].eq_ignore_ascii_case("MINVALUE") {
                min_val = Some(1);
                idx += 2;
            } else if tok == "MAXVALUE" {
                idx += 1;
                if idx < tokens.len() {
                    let v = tokens[idx].parse::<i64>().map_err(|e| H2Error::SqlParse(e.to_string()))?;
                    max_val = Some(v);
                    idx += 1;
                }
            } else if tok == "NO" && idx + 1 < tokens.len() && tokens[idx + 1].eq_ignore_ascii_case("MAXVALUE") {
                max_val = Some(i64::MAX);
                idx += 2;
            } else if tok == "CYCLE" {
                cycle = Some(true);
                idx += 1;
            } else if tok == "NO" && idx + 1 < tokens.len() && tokens[idx + 1].eq_ignore_ascii_case("CYCLE") {
                cycle = Some(false);
                idx += 2;
            } else {
                idx += 1;
            }
        }

        match self.catalog.alter_sequence(seq_name, restart_val, increment_by, min_val, max_val, cycle) {
            Ok(_) => {
                self.store.commit()?;
                Ok(ExecutionResult::Ddl)
            }
            Err(e) => {
                if if_exists && e.to_string().contains("not found") {
                    Ok(ExecutionResult::Ddl)
                } else {
                    Err(e)
                }
            }
        }
    }

    fn execute_create_queue_table(&self, _tx: &Transaction, sql: &str) -> H2Result<ExecutionResult> {
        if self.is_read_only() {
            return Err(H2Error::ReadOnly(
                "Cannot execute data-modifying or DDL statements on a read-only instance".to_string(),
            ));
        }

        let (cleaned_sql, retention_ms, max_bytes) = parse_queue_with_clause(sql)?;

        let regex_cqt = regex::Regex::new(r"(?i)^CREATE\s+QUEUE\s+TABLE").unwrap();
        let create_table_sql = regex_cqt.replace(&cleaned_sql, "CREATE TABLE").to_string();

        let statements = parse_sql(&create_table_sql)?;
        let stmt = statements.into_iter().next().ok_or_else(|| {
            H2Error::Execution("Failed to parse CREATE QUEUE TABLE".to_string())
        })?;

        match stmt {
            Statement::CreateTable(create_table) => {
                let table_def = extract_create_table(
                    &create_table.name,
                    &create_table.columns,
                    &create_table.constraints,
                )?;
                let tbl_name = table_def.name.clone();
                let queue_def = TableDef::new_queue(
                    tbl_name.clone(),
                    table_def.columns,
                    retention_ms,
                    max_bytes,
                );

                if let Err(e) = self.catalog.create_table(queue_def) {
                    if create_table.if_not_exists && e.to_string().contains("already exists") {
                        return Ok(ExecutionResult::Ddl);
                    }
                    return Err(e);
                }

                // 主キー用ユニークインデックス作成 (_offset)
                let pk_index_name = format!("pk_{}", tbl_name.to_lowercase());
                let _ = self.catalog.create_index(IndexDef {
                    name: pk_index_name,
                    table_name: tbl_name.clone(),
                    columns: vec!["_offset".to_string()],
                    is_unique: true,
                });

                self.store.commit()?;
                Ok(ExecutionResult::Ddl)
            }
            _ => Err(H2Error::Execution("Expected CREATE TABLE statement".to_string())),
        }
    }

    fn execute_analyze(&self, tx: &Transaction, sql: &str) -> H2Result<ExecutionResult> {
        let parts: Vec<&str> = sql.split_whitespace().collect();
        if parts.len() == 1 {
            // 全テーブルを ANALYZE
            let tables = self.catalog.all_tables();
            for table_def in tables {
                let (stats, col_stats) = crate::stats::analyze_table(tx, &table_def)?;
                self.catalog.update_table_stats(&table_def.name, stats, col_stats)?;
            }
        } else {
            // 指定テーブルを ANALYZE
            let table_name = parts[1].trim_matches(';').trim_matches('"').trim_matches('\'');
            let table_def = self.catalog.get_table(table_name)
                .ok_or_else(|| H2Error::Catalog(format!("Table '{}' not found", table_name)))?;
            let (stats, col_stats) = crate::stats::analyze_table(tx, &table_def)?;
            self.catalog.update_table_stats(table_name, stats, col_stats)?;
        }
        self.store.commit()?;
        Ok(ExecutionResult::Ddl)
    }

    fn execute_set_max_materialized_rows(&self, sql: &str) -> H2Result<ExecutionResult> {
        let val_part = sql.split('=').nth(1)
            .or_else(|| sql.split_whitespace().nth(2))
            .ok_or_else(|| H2Error::Execution("Expected value in SET max_materialized_rows = <num>".to_string()))?;
        let trimmed_val = val_part.trim().trim_matches(';').trim_matches('\'').trim_matches('"');
        let rows = trimmed_val.parse::<usize>()
            .map_err(|_| H2Error::Execution(format!("Invalid integer for max_materialized_rows: {}", trimmed_val)))?;
        self.memory_config.set_max_materialized_rows(rows);
        Ok(ExecutionResult::Ddl)
    }

    fn execute_set_work_mem(&self, sql: &str) -> H2Result<ExecutionResult> {
        let val_part = sql.split('=').nth(1)
            .or_else(|| sql.split_whitespace().nth(2))
            .ok_or_else(|| H2Error::Execution("Expected value in SET work_mem = <size>".to_string()))?;
        let trimmed_val = val_part.trim().trim_matches(';').trim_matches('\'').trim_matches('"').to_uppercase();
        let bytes = if trimmed_val.ends_with("MB") {
            let num = trimmed_val[..trimmed_val.len() - 2].trim().parse::<usize>()
                .map_err(|_| H2Error::Execution(format!("Invalid size for work_mem: {}", trimmed_val)))?;
            num * 1024 * 1024
        } else if trimmed_val.ends_with("KB") {
            let num = trimmed_val[..trimmed_val.len() - 2].trim().parse::<usize>()
                .map_err(|_| H2Error::Execution(format!("Invalid size for work_mem: {}", trimmed_val)))?;
            num * 1024
        } else if trimmed_val.ends_with('B') {
            trimmed_val[..trimmed_val.len() - 1].trim().parse::<usize>()
                .map_err(|_| H2Error::Execution(format!("Invalid size for work_mem: {}", trimmed_val)))?
        } else {
            trimmed_val.parse::<usize>()
                .map_err(|_| H2Error::Execution(format!("Invalid size for work_mem: {}", trimmed_val)))?
        };
        self.memory_config.set_work_mem(bytes);
        Ok(ExecutionResult::Ddl)
    }

    fn execute_set_execution_mode(&self, sql: &str) -> H2Result<ExecutionResult> {
        let val_part = sql.split('=').nth(1)
            .or_else(|| sql.split_whitespace().nth(2))
            .ok_or_else(|| H2Error::Execution("Expected value in SET execution_mode = 'auto'|'row'|'vectorized'".to_string()))?;
        let mode = val_part.trim().trim_matches(';').trim_matches('\'').trim_matches('"').to_lowercase();
        match mode.as_str() {
            "auto" | "row" | "vectorized" => {
                self.set_execution_mode(&mode);
                Ok(ExecutionResult::Ddl)
            }
            _ => Err(H2Error::Execution(format!(
                "Invalid execution mode '{}'. Expected 'auto', 'row', or 'vectorized'",
                mode
            ))),
        }
    }

    fn parse_aggregate_ops(
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

    #[allow(dead_code)]
    pub(crate) fn try_execute_pushdown_aggregate<'a, I>(
        ops: &[crate::vectorized::VectorAggregateOp],
        col_names: &[String],
        raw_val_bytes_iter: I,
        table_def: &TableDef,
    ) -> ExecutionResult
    where
        I: IntoIterator<Item = &'a [u8]>,
    {
        let mut sums = vec![0.0f64; ops.len()];
        let mut counts = vec![0i64; ops.len()];
        let mut mins = vec![f64::MAX; ops.len()];
        let mut maxs = vec![f64::MIN; ops.len()];
        let mut min_ints = vec![i64::MAX; ops.len()];
        let mut max_ints = vec![i64::MIN; ops.len()];

        for val_bytes in raw_val_bytes_iter {
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
        }

        Self::build_pushdown_aggregate_result(ops, col_names, &counts, &sums, &mins, &maxs, &min_ints, &max_ints, table_def)
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

    fn try_execute_vectorized_aggregate(
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

    /// キューテーブルの保持ポリシー（RETENTION_TIME, MAX_BYTES）に基づき古いメッセージをパージ（Head Truncation）
    pub fn purge_queue_retention(&self, tx: &Transaction, table_name: &str) -> H2Result<usize> {
        let table_def = match self.catalog.get_table(table_name) {
            Some(t) => t,
            None => return Ok(0),
        };
        if !table_def.is_queue {
            return Ok(0);
        }
        if table_def.retention_duration_ms.is_none() && table_def.max_bytes.is_none() {
            return Ok(0);
        }

        let map_name = table_def.map_name();
        let entries = tx.scan_visible(&map_name)?;
        if entries.is_empty() {
            return Ok(0);
        }

        struct QueueRowItem {
            key: Vec<u8>,
            row_id: u64,
            offset: i64,
            timestamp_ms: i64,
            byte_size: usize,
            row: Row,
        }

        let mut items = Vec::with_capacity(entries.len());
        let mut total_bytes: u64 = 0;

        for (k, val_bytes) in entries {
            let byte_size = k.len() + val_bytes.len();
            total_bytes += byte_size as u64;
            let mut row = Row::from_bytes(&val_bytes)?;
            table_def.align_row(&mut row);

            let row_id = if k.len() == 8 {
                u64::from_le_bytes(k.as_slice().try_into().unwrap())
            } else {
                0
            };

            let offset = match row.values.get(0) {
                Some(Value::BigInt(v)) => *v,
                _ => row_id as i64,
            };

            let timestamp_ms = match row.values.get(1) {
                Some(Value::Timestamp(ts)) => ts.timestamp_millis(),
                _ => 0,
            };


            items.push(QueueRowItem {
                key: k,
                row_id,
                offset,
                timestamp_ms,
                byte_size,
                row,
            });
        }

        // offset 昇順にソート
        items.sort_by_key(|it| it.offset);

        let now_ms = chrono::Utc::now().timestamp_millis();
        let mut to_purge_keys: Vec<(Vec<u8>, u64, Row, usize)> = Vec::new();
        let mut purged_key_set: std::collections::HashSet<Vec<u8>> = std::collections::HashSet::new();

        // 1. 時間基準パージ
        if let Some(retention_ms) = table_def.retention_duration_ms {
            let cutoff = now_ms.saturating_sub(retention_ms as i64);
            for item in &items {
                if item.timestamp_ms < cutoff {
                    if purged_key_set.insert(item.key.clone()) {
                        to_purge_keys.push((item.key.clone(), item.row_id, item.row.clone(), item.byte_size));
                    }
                }
            }
        }

        // 2. 容量基準パージ
        if let Some(max_b) = table_def.max_bytes {
            let mut current_bytes = total_bytes;
            for (_, _, _, size) in &to_purge_keys {
                current_bytes = current_bytes.saturating_sub(*size as u64);
            }

            if current_bytes > max_b {
                for item in &items {
                    if current_bytes <= max_b {
                        break;
                    }
                    if purged_key_set.insert(item.key.clone()) {
                        current_bytes = current_bytes.saturating_sub(item.byte_size as u64);
                        to_purge_keys.push((item.key.clone(), item.row_id, item.row.clone(), item.byte_size));
                    }
                }
            }
        }

        let purged_count = to_purge_keys.len();
        if purged_count == 0 {
            return Ok(0);
        }

        let indexes = self.catalog.get_table_indexes(table_name);
        for (k, r_id, row, _) in to_purge_keys {
            for idx in &indexes {
                if let Some(vals) = get_index_values(&table_def, idx, &row) {
                    let idx_map_name = format!("idx_{}_{}", table_def.name.to_lowercase(), idx.name.to_lowercase());
                    let idx_key = encode_composite_index_key(&vals, r_id);
                    let _ = tx.remove(&idx_map_name, &idx_key);
                }
            }
            tx.remove(&map_name, &k)?;
        }

        Ok(purged_count)
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

    fn resolve_virtual_graph_table(
        &self,
        tx: &Transaction,
        table_name: &str,
        table_alias: Option<String>,
    ) -> H2Result<Option<(TableDef, Vec<Row>, Option<String>)>> {
        let (graph_name, kind) = match crate::catalog::parse_virtual_graph_table(table_name) {
            Some(res) => res,
            None => return Ok(None),
        };

        let mut table_def = match self.catalog.get_table(table_name) {
            Some(t) => t,
            None => return Ok(None),
        };

        if let Some(ref a) = table_alias {
            table_def.name = a.clone();
        }

        let rows = match kind {
            crate::catalog::VirtualGraphKind::Nodes => {
                let map_name = format!("_g_{}_nodes", graph_name);
                let entries = tx.scan_visible(&map_name)?;
                let mut rows = Vec::with_capacity(entries.len());
                for (_k, val_bytes) in entries {
                    if let Ok(node) = bincode::deserialize::<h2_graph::Node>(&val_bytes) {
                        let labels_val = Value::Array(node.labels.into_iter().map(Value::String).collect());
                        let mut prop_map = serde_json::Map::new();
                        for (k, v) in node.properties {
                            prop_map.insert(k, serde_json::to_value(&v).unwrap_or(serde_json::Value::Null));
                        }
                        let prop_val = Value::Json(serde_json::Value::Object(prop_map));
                        rows.push(Row::new(vec![
                            Value::BigInt(node.id as i64),
                            labels_val,
                            prop_val,
                        ]));
                    }
                }
                rows
            }
            crate::catalog::VirtualGraphKind::Edges => {
                let map_name = format!("_g_{}_edges", graph_name);
                let entries = tx.scan_visible(&map_name)?;
                let mut rows = Vec::with_capacity(entries.len());
                for (_k, val_bytes) in entries {
                    if let Ok(edge) = bincode::deserialize::<h2_graph::Edge>(&val_bytes) {
                        let mut prop_map = serde_json::Map::new();
                        for (k, v) in edge.properties {
                            prop_map.insert(k, serde_json::to_value(&v).unwrap_or(serde_json::Value::Null));
                        }
                        let prop_val = Value::Json(serde_json::Value::Object(prop_map));
                        rows.push(Row::new(vec![
                            Value::BigInt(edge.id as i64),
                            Value::BigInt(edge.src_id as i64),
                            Value::BigInt(edge.dst_id as i64),
                            Value::String(edge.edge_type),
                            prop_val,
                        ]));
                    }
                }
                rows
            }
        };

        Ok(Some((table_def, rows, table_alias)))
    }

    fn resolve_cypher_table_function(
        &self,
        tx: &Transaction,
        name: &sqlparser::ast::ObjectName,
        alias: &Option<sqlparser::ast::TableAlias>,
        args: &Option<sqlparser::ast::TableFunctionArgs>,
    ) -> H2Result<Option<(TableDef, Vec<Row>, Option<String>)>> {
        let func_name = name.to_string();
        if !func_name.eq_ignore_ascii_case("cypher") {
            return Ok(None);
        }

        let func_args = match args {
            Some(fa) => &fa.args,
            None => {
                return Err(H2Error::Execution(
                    "CYPHER() table function requires at least 2 arguments: graph_name and cypher_query".to_string(),
                ))
            }
        };

        if func_args.len() < 2 {
            return Err(H2Error::Execution(
                "CYPHER() table function requires at least 2 arguments: graph_name and cypher_query".to_string(),
            ));
        }

        fn extract_str_arg(arg: &sqlparser::ast::FunctionArg) -> H2Result<String> {
            match arg {
                sqlparser::ast::FunctionArg::Unnamed(sqlparser::ast::FunctionArgExpr::Expr(expr)) => {
                    match expr {
                        sqlparser::ast::Expr::Value(sqlparser::ast::Value::SingleQuotedString(s)) => {
                            Ok(s.clone())
                        }
                        sqlparser::ast::Expr::Value(sqlparser::ast::Value::DoubleQuotedString(s)) => {
                            Ok(s.clone())
                        }
                        sqlparser::ast::Expr::Identifier(ident) => Ok(ident.value.clone()),
                        _ => Err(H2Error::Execution(format!(
                            "Expected string literal argument to CYPHER(), found {:?}",
                            expr
                        ))),
                    }
                }
                _ => Err(H2Error::Execution(
                    "Expected unnamed string argument to CYPHER()".to_string(),
                )),
            }
        }

        let graph_name = extract_str_arg(&func_args[0])?;
        let cypher_query = extract_str_arg(&func_args[1])?;

        let mut params = HashMap::new();
        if func_args.len() >= 3 {
            if let Ok(params_str) = extract_str_arg(&func_args[2]) {
                if let Ok(json_val) = serde_json::from_str::<serde_json::Value>(&params_str) {
                    if let serde_json::Value::Object(map) = json_val {
                        for (k, v) in map {
                            let gv = match v {
                                serde_json::Value::Null => h2_graph::GraphValue::Null,
                                serde_json::Value::Bool(b) => h2_graph::GraphValue::Boolean(b),
                                serde_json::Value::Number(n) => {
                                    if let Some(i) = n.as_i64() {
                                        h2_graph::GraphValue::Integer(i)
                                    } else {
                                        h2_graph::GraphValue::Float(n.as_f64().unwrap_or(0.0))
                                    }
                                }
                                serde_json::Value::String(s) => h2_graph::GraphValue::String(s),
                                _ => h2_graph::GraphValue::String(v.to_string()),
                            };
                            params.insert(k, gv);
                        }
                    }
                }
            }
        }

        let engine = h2_graph::GraphEngine::new(self.store.clone(), &graph_name)?;
        let result = engine.execute_on_tx(tx, &cypher_query, &params)?;

        let table_alias_name = alias
            .as_ref()
            .map(|a| a.name.value.clone())
            .unwrap_or_else(|| "cypher".to_string());

        let col_names = if let Some(ref a) = alias {
            if !a.columns.is_empty() {
                a.columns.iter().map(|c| c.name.value.clone()).collect()
            } else {
                result.columns.clone()
            }
        } else {
            result.columns.clone()
        };

        let col_defs: Vec<ColumnDef> = col_names
            .into_iter()
            .map(|col_name| ColumnDef::new(col_name, h2_types::DataType::VarChar(None), true, false))
            .collect();

        let mut rows = Vec::with_capacity(result.rows.len());
        for row in result.rows {
            let row_values: Vec<Value> = row.into_iter().map(|gv| gv.to_value()).collect();
            rows.push(Row::new(row_values));
        }

        let table_def = TableDef::new(table_alias_name.clone(), col_defs);
        Ok(Some((table_def, rows, Some(table_alias_name))))
    }

    fn resolve_pg_proc_table(
        &self,
        table_name: &str,
        table_alias: Option<String>,
    ) -> (crate::catalog::TableDef, Vec<Row>, Option<String>) {
        let t_def = crate::catalog::TableDef::new(
            table_name.to_string(),
            vec![
                crate::catalog::ColumnDef::new("oid", h2_types::DataType::BigInt, false, true),
                crate::catalog::ColumnDef::new("proname", h2_types::DataType::VarChar(None), false, false),
                crate::catalog::ColumnDef::new("prokind", h2_types::DataType::VarChar(None), false, false),
                crate::catalog::ColumnDef::new("prorettype", h2_types::DataType::VarChar(None), false, false),
                crate::catalog::ColumnDef::new("pronargs", h2_types::DataType::Integer, false, false),
                crate::catalog::ColumnDef::new("proargnames", h2_types::DataType::VarChar(None), false, false),
                crate::catalog::ColumnDef::new("prosrc", h2_types::DataType::VarChar(None), false, false),
            ],
        );

        let mut rows = Vec::new();
        let mut oid = 10001i64;

        // 登録済みユーザー定義ルーチン
        for r in self.catalog.all_routines() {
            let kind_str = match r.kind {
                crate::procedural::RoutineKind::Function => "f",
                crate::procedural::RoutineKind::Procedure => "p",
            };
            let ret_str = r.return_type.map(|t| t.to_string().to_lowercase()).unwrap_or_else(|| "void".to_string());
            let arg_names = r.parameters.iter().map(|p| p.name.clone()).collect::<Vec<_>>().join(",");

            rows.push(Row::new(vec![
                Value::BigInt(oid),
                Value::String(r.name),
                Value::String(kind_str.to_string()),
                Value::String(ret_str),
                Value::Integer(r.parameters.len() as i32),
                Value::String(arg_names),
                Value::String(r.source_sql),
            ]));
            oid += 1;
        }

        (t_def, rows, table_alias)
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
            TableFactor::Table { name, alias, args, .. } => {
                if let Some(res) = self.resolve_cypher_table_function(tx, name, alias, args)? {
                    return Ok(res);
                }
                let table_name = normalize_object_name(name);
                let table_alias = alias.as_ref().map(|a| a.name.value.clone());

                if let Some(res) = self.resolve_virtual_graph_table(tx, &table_name, table_alias.clone())? {
                    return Ok(res);
                }

                if let Some((cte_def, cte_rows)) = current_ctes.get(&table_name.to_lowercase()) {
                    let mut t_def = cte_def.clone();
                    if let Some(ref a) = table_alias {
                        t_def.name = a.clone();
                    }
                    Ok((t_def, cte_rows.clone(), table_alias))
                } else if let Some(view) = self.catalog.get_view(&table_name) {
                    self.resolve_view_query(tx, &view, table_alias, current_ctes)
                } else {
                    let is_pg_proc = table_name.eq_ignore_ascii_case("pg_proc")
                        || table_name.eq_ignore_ascii_case("pg_catalog.pg_proc");
                    if is_pg_proc {
                        return Ok(self.resolve_pg_proc_table(&table_name, table_alias));
                    }

                    if table_name.eq_ignore_ascii_case("dual") {
                        let t_def = crate::catalog::TableDef::new(
                            table_name.clone(),
                            vec![
                                crate::catalog::ColumnDef::new("dummy", h2_types::DataType::VarChar(Some(1)), false, false),
                            ],
                        );
                        let rows = vec![Row::new(vec![Value::String("X".to_string())])];
                        return Ok((t_def, rows, table_alias));
                    }

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
                                            let target_ctx = RowContext::from_table_def(&table_def, target_alias.as_deref());
                                            for (_col_name, col_idx, val_expr) in &parsed_assignments {
                                                let new_val = evaluate_expr_context(val_expr, &target_ctx, &row)?;
                                                let casted = new_val.cast_to(&table_def.columns[*col_idx].data_type)?;
                                                row.values[*col_idx] = casted;
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

fn matches_index_condition(expr: &Expr, target_col: &str) -> bool {
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

fn extract_equality_predicate(expr: &Expr) -> Option<(String, Value)> {
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

fn build_index_filter(expr: &Expr, target_col: &str) -> Option<Box<dyn Fn(&Value) -> bool>> {
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
        let _grant = self.admission.acquire_grant(64 * 1024, std::time::Duration::from_secs(5))?;
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
                                    let row_cnt = t.stats.as_ref().map(|s| s.row_count as i64).unwrap_or(t.approx_row_count);
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

    fn execute_create_user(&self, sql: &str) -> H2Result<ExecutionResult> {
        let trimmed = sql.trim().trim_end_matches(';').trim();
        let upper = trimmed.to_uppercase();
        let rest = if upper.starts_with("CREATE USER IF NOT EXISTS ") {
            &trimmed["CREATE USER IF NOT EXISTS ".len()..]
        } else if upper.starts_with("CREATE USER ") {
            &trimmed["CREATE USER ".len()..]
        } else {
            return Err(H2Error::SqlParse("Invalid CREATE USER syntax".to_string()));
        };

        let mut parts = rest.split_whitespace();
        let username = parts.next().ok_or_else(|| H2Error::SqlParse("Missing username in CREATE USER".to_string()))?
            .trim_matches('\'').trim_matches('"');

        let mut password = None;
        let mut host = None;
        let is_superuser = rest.to_uppercase().contains("SUPERUSER");

        let upper_rest = rest.to_uppercase();
        if let Some(pos) = upper_rest.find("PASSWORD") {
            let sub = rest[pos + "PASSWORD".len()..].trim();
            if let Some(start) = sub.find('\'') {
                if let Some(end) = sub[start + 1..].find('\'') {
                    password = Some(&sub[start + 1..start + 1 + end]);
                }
            }
        }

        if let Some(pos) = upper_rest.find("HOST") {
            let sub = rest[pos + "HOST".len()..].trim();
            if let Some(start) = sub.find('\'') {
                if let Some(end) = sub[start + 1..].find('\'') {
                    host = Some(&sub[start + 1..start + 1 + end]);
                }
            }
        }

        self.auth.create_user(username, password, host, is_superuser)?;
        Ok(ExecutionResult::Ddl)
    }

    fn execute_alter_user(&self, sql: &str) -> H2Result<ExecutionResult> {
        let trimmed = sql.trim().trim_end_matches(';').trim();
        let rest = trimmed["ALTER USER ".len()..].trim();
        let username = rest.split_whitespace().next().ok_or_else(|| H2Error::SqlParse("Missing username in ALTER USER".to_string()))?
            .trim_matches('\'').trim_matches('"');

        let mut password = None;
        let mut host = None;
        let upper_rest = rest.to_uppercase();

        if let Some(pos) = upper_rest.find("PASSWORD") {
            let sub = rest[pos + "PASSWORD".len()..].trim();
            if let Some(start) = sub.find('\'') {
                if let Some(end) = sub[start + 1..].find('\'') {
                    password = Some(&sub[start + 1..start + 1 + end]);
                }
            }
        }

        if let Some(pos) = upper_rest.find("HOST") {
            let sub = rest[pos + "HOST".len()..].trim();
            if let Some(start) = sub.find('\'') {
                if let Some(end) = sub[start + 1..].find('\'') {
                    host = Some(&sub[start + 1..start + 1 + end]);
                }
            }
        }

        self.auth.alter_user(username, password, host)?;
        Ok(ExecutionResult::Ddl)
    }

    fn execute_drop_user(&self, sql: &str) -> H2Result<ExecutionResult> {
        let trimmed = sql.trim().trim_end_matches(';').trim();
        let upper = trimmed.to_uppercase();
        let (username, if_exists) = if upper.starts_with("DROP USER IF EXISTS ") {
            (&trimmed["DROP USER IF EXISTS ".len()..].trim(), true)
        } else {
            (&trimmed["DROP USER ".len()..].trim(), false)
        };
        let name = username.trim_matches('\'').trim_matches('"');
        self.auth.drop_user(name, if_exists)?;
        Ok(ExecutionResult::Ddl)
    }

    fn execute_grant(&self, sql: &str) -> H2Result<ExecutionResult> {
        let trimmed = sql.trim().trim_end_matches(';').trim();
        let upper = trimmed.to_uppercase();
        let on_pos = upper.find(" ON ").ok_or_else(|| H2Error::SqlParse("Missing ON in GRANT statement".to_string()))?;
        let to_pos = upper.find(" TO ").ok_or_else(|| H2Error::SqlParse("Missing TO in GRANT statement".to_string()))?;

        let privs_str = trimmed["GRANT ".len()..on_pos].trim();
        let table_raw = trimmed[on_pos + " ON ".len()..to_pos].trim();
        let table_name = if table_raw.to_uppercase().starts_with("TABLE ") {
            table_raw["TABLE ".len()..].trim()
        } else {
            table_raw
        }.trim_matches('\'').trim_matches('"');

        let username = trimmed[to_pos + " TO ".len()..].trim().trim_matches('\'').trim_matches('"');

        let mut privileges = Vec::new();
        for p in privs_str.split(',') {
            privileges.push(crate::auth::Privilege::from_str(p.trim())?);
        }

        self.auth.grant(username, table_name, privileges)?;
        Ok(ExecutionResult::Ddl)
    }

    fn execute_revoke(&self, sql: &str) -> H2Result<ExecutionResult> {
        let trimmed = sql.trim().trim_end_matches(';').trim();
        let upper = trimmed.to_uppercase();
        let on_pos = upper.find(" ON ").ok_or_else(|| H2Error::SqlParse("Missing ON in REVOKE statement".to_string()))?;
        let from_pos = upper.find(" FROM ").ok_or_else(|| H2Error::SqlParse("Missing FROM in REVOKE statement".to_string()))?;

        let privs_str = trimmed["REVOKE ".len()..on_pos].trim();
        let table_raw = trimmed[on_pos + " ON ".len()..from_pos].trim();
        let table_name = if table_raw.to_uppercase().starts_with("TABLE ") {
            table_raw["TABLE ".len()..].trim()
        } else {
            table_raw
        }.trim_matches('\'').trim_matches('"');

        let username = trimmed[from_pos + " FROM ".len()..].trim().trim_matches('\'').trim_matches('"');

        let mut privileges = Vec::new();
        for p in privs_str.split(',') {
            privileges.push(crate::auth::Privilege::from_str(p.trim())?);
        }

        self.auth.revoke(username, table_name, privileges)?;
        Ok(ExecutionResult::Ddl)
    }

    fn execute_show_users(&self) -> H2Result<ExecutionResult> {
        let users = self.auth.list_users();
        let columns = vec!["username".to_string(), "allowed_hosts".to_string(), "is_superuser".to_string()];
        let mut rows = Vec::new();
        for u in users {
            let hosts = u.allowed_hosts.join(", ");
            let superuser_str = if u.is_superuser { "true" } else { "false" };
            rows.push(crate::row::Row::new(vec![
                Value::String(u.username),
                Value::String(hosts),
                Value::String(superuser_str.to_string()),
            ]));
        }
        Ok(ExecutionResult::Query { columns, rows })
    }

    fn execute_show_grants(&self, user: &str) -> H2Result<ExecutionResult> {
        let grants = self.auth.get_user_grants(user)?;
        let columns = vec!["table_name".to_string(), "privileges".to_string()];
        let mut rows = Vec::new();
        for (tbl, privs) in grants {
            let priv_str = privs.iter().map(|p: &crate::auth::Privilege| p.to_string()).collect::<Vec<_>>().join(", ");
            rows.push(crate::row::Row::new(vec![
                Value::String(tbl),
                Value::String(priv_str),
            ]));
        }
        Ok(ExecutionResult::Query { columns, rows })
    }

    fn check_statement_privileges(&self, user: &str, stmt: &Statement) -> H2Result<()> {
        if !self.auth.is_auth_enabled() {
            return Ok(());
        }

        let users = self.auth.list_users();
        if let Some(u) = users.into_iter().find(|u| u.username.eq_ignore_ascii_case(user)) {
            if u.is_superuser {
                return Ok(());
            }
        }

        match stmt {
            Statement::Query(query) => {
                let tables = extract_tables_from_query(query);
                for t in tables {
                    self.auth.check_privilege(user, &t, crate::auth::Privilege::Select)?;
                }
            }
            Statement::Insert(insert) => {
                let t = normalize_object_name(&insert.table_name);
                self.auth.check_privilege(user, &t, crate::auth::Privilege::Insert)?;
                if let Some(ref src) = insert.source {
                    let select_tables = extract_tables_from_query(src);
                    for st in select_tables {
                        self.auth.check_privilege(user, &st, crate::auth::Privilege::Select)?;
                    }
                }
            }
            Statement::Update { table, .. } => {
                let t = match &table.relation {
                    TableFactor::Table { name, .. } => normalize_object_name(name),
                    _ => table.relation.to_string(),
                };
                self.auth.check_privilege(user, &t, crate::auth::Privilege::Update)?;
            }
            Statement::Delete(delete) => {
                let from_table = match &delete.from {
                    sqlparser::ast::FromTable::WithFromKeyword(tables) => tables.first(),
                    sqlparser::ast::FromTable::WithoutKeyword(tables) => tables.first(),
                };
                if let Some(from_tbl) = from_table {
                    let t = match &from_tbl.relation {
                        TableFactor::Table { name, .. } => normalize_object_name(name),
                        _ => from_tbl.relation.to_string(),
                    };
                    self.auth.check_privilege(user, &t, crate::auth::Privilege::Delete)?;
                }
            }
            Statement::CreateTable { .. }
            | Statement::AlterTable { .. }
            | Statement::Drop { .. }
            | Statement::Truncate { .. } => {
                return Err(H2Error::PermissionDenied(format!(
                    "Permission denied: User '{}' does not have administrative privileges for DDL", user
                )));
            }
            _ => {}
        }
        Ok(())
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



fn extract_file_path(param: &str) -> String {
    let p = param.trim().trim_end_matches(';').trim();
    p.trim_matches(|c| c == '\'' || c == '"' || c == '`').to_string()
}

fn parse_restore_pitr_options(opts: &str) -> H2Result<(Option<String>, h2_mvstore::RecoveryTarget)> {
    let mut wal_archive = None;
    let mut target = h2_mvstore::RecoveryTarget::Latest;

    let parts = opts.split(',').collect::<Vec<_>>();
    for part in parts {
        let trimmed = part.trim();
        if let Some(eq_idx) = trimmed.find('=') {
            let key = trimmed[..eq_idx].trim().to_uppercase();
            let val = trimmed[eq_idx + 1..].trim().trim_matches('\'').trim_matches('"');
            match key.as_str() {
                "WAL_ARCHIVE" => wal_archive = Some(val.to_string()),
                "RECOVERY_TARGET_VERSION" => {
                    let v: u64 = val.parse().map_err(|e| H2Error::Execution(format!("Invalid version: {}", e)))?;
                    target = h2_mvstore::RecoveryTarget::Version(v);
                }
                "RECOVERY_TARGET_TX" => {
                    let tx: u64 = val.parse().map_err(|e| H2Error::Execution(format!("Invalid tx id: {}", e)))?;
                    target = h2_mvstore::RecoveryTarget::TransactionId(tx);
                }
                "RECOVERY_TARGET_TIME" => {
                    if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(val, "%Y-%m-%d %H:%M:%S") {
                        let nanos = dt.and_utc().timestamp_nanos_opt().unwrap_or(0);
                        target = h2_mvstore::RecoveryTarget::TimestampNanos(nanos);
                    } else if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(val) {
                        let nanos = dt.timestamp_nanos_opt().unwrap_or(0);
                        target = h2_mvstore::RecoveryTarget::TimestampNanos(nanos);
                    } else if let Ok(nanos) = val.parse::<i64>() {
                        target = h2_mvstore::RecoveryTarget::TimestampNanos(nanos);
                    }
                }
                _ => {}
            }
        }
    }

    Ok((wal_archive, target))
}

fn split_sql_script(content: &str) -> Vec<String> {
    let mut stmts = Vec::new();
    let mut current = String::new();
    let mut in_single_quote = false;
    let mut prev_char = '\0';

    for ch in content.chars() {
        if ch == '\'' && prev_char != '\\' {
            in_single_quote = !in_single_quote;
            current.push(ch);
        } else if ch == ';' && !in_single_quote {
            let trimmed = current.trim();
            if !trimmed.is_empty() {
                stmts.push(trimmed.to_string());
            }
            current.clear();
        } else {
            current.push(ch);
        }
        prev_char = ch;
    }
    let trimmed = current.trim();
    if !trimmed.is_empty() {
        stmts.push(trimmed.to_string());
    }
    stmts
}

fn format_csv_field(val: &Value, delimiter: char) -> String {
    let s = match val {
        Value::Null => "".to_string(),
        Value::String(str_val) => str_val.clone(),
        _ => val.to_string(),
    };
    if s.contains(delimiter) || s.contains('"') || s.contains('\n') || s.contains('\r') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s
    }
}

fn parse_csv_line(line: &str, delimiter: char) -> Vec<String> {
    let mut fields = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut chars = line.chars().peekable();

    while let Some(ch) = chars.next() {
        if in_quotes {
            if ch == '"' {
                if chars.peek() == Some(&'"') {
                    current.push('"');
                    chars.next();
                } else {
                    in_quotes = false;
                }
            } else {
                current.push(ch);
            }
        } else {
            if ch == '"' {
                in_quotes = true;
            } else if ch == delimiter {
                fields.push(current.clone());
                current.clear();
            } else {
                current.push(ch);
            }
        }
    }
    fields.push(current);
    fields
}

fn parse_duration_to_ms(s: &str) -> H2Result<u64> {
    let s = s.trim().trim_matches('\'').trim_matches('"').trim();
    let parts: Vec<&str> = s.split_whitespace().collect();
    if parts.is_empty() {
        return Err(H2Error::Execution("Empty RETENTION_TIME".to_string()));
    }
    let (num_str, unit_str) = if parts.len() == 1 {
        let val = parts[0];
        let num_end = val.find(|c: char| !c.is_numeric() && c != '.').unwrap_or(val.len());
        (&val[..num_end], &val[num_end..])
    } else {
        (parts[0], parts[1])
    };

    let num: f64 = num_str.parse().map_err(|_| {
        H2Error::Execution(format!("Invalid number in RETENTION_TIME: '{}'", num_str))
    })?;

    let unit = unit_str.to_ascii_uppercase();
    let multiplier = match unit.as_str() {
        "" | "MS" | "MILLIS" | "MILLISECOND" | "MILLISECONDS" => 1.0,
        "S" | "SEC" | "SECS" | "SECOND" | "SECONDS" => 1000.0,
        "M" | "MIN" | "MINS" | "MINUTE" | "MINUTES" => 60_000.0,
        "H" | "HR" | "HRS" | "HOUR" | "HOURS" => 3_600_000.0,
        "D" | "DAY" | "DAYS" => 86_400_000.0,
        _ => return Err(H2Error::Execution(format!("Unknown time unit in RETENTION_TIME: '{}'", unit_str))),
    };

    Ok((num * multiplier) as u64)
}

fn parse_bytes_to_u64(s: &str) -> H2Result<u64> {
    let s = s.trim().trim_matches('\'').trim_matches('"').trim();
    let parts: Vec<&str> = s.split_whitespace().collect();
    if parts.is_empty() {
        return Err(H2Error::Execution("Empty MAX_BYTES".to_string()));
    }
    let (num_str, unit_str) = if parts.len() == 1 {
        let val = parts[0];
        let num_end = val.find(|c: char| !c.is_numeric() && c != '.').unwrap_or(val.len());
        (&val[..num_end], &val[num_end..])
    } else {
        (parts[0], parts[1])
    };

    let num: f64 = num_str.parse().map_err(|_| {
        H2Error::Execution(format!("Invalid number in MAX_BYTES: '{}'", num_str))
    })?;

    let unit = unit_str.to_ascii_uppercase();
    let multiplier: f64 = match unit.as_str() {
        "" | "B" | "BYTE" | "BYTES" => 1.0,
        "K" | "KB" | "KIB" => 1024.0,
        "M" | "MB" | "MIB" => 1024.0 * 1024.0,
        "G" | "GB" | "GIB" => 1024.0 * 1024.0 * 1024.0,
        "T" | "TB" | "TIB" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
        _ => return Err(H2Error::Execution(format!("Unknown bytes unit in MAX_BYTES: '{}'", unit_str))),
    };

    Ok((num * multiplier) as u64)
}

fn parse_queue_with_clause(sql: &str) -> H2Result<(String, Option<u64>, Option<u64>)> {
    let upper = sql.to_uppercase();
    if let Some(pos) = upper.rfind("WITH") {
        let before = sql[..pos].trim();
        let after = sql[pos + 4..].trim();
        if after.starts_with('(') && after.ends_with(')') {
            let inner = after[1..after.len() - 1].trim();
            let mut retention_ms = None;
            let mut max_bytes = None;

            for item in inner.split(',') {
                let kv: Vec<&str> = item.splitn(2, '=').collect();
                if kv.len() == 2 {
                    let key = kv[0].trim().to_ascii_uppercase();
                    let val = kv[1].trim();
                    if key == "RETENTION_TIME" {
                        retention_ms = Some(parse_duration_to_ms(val)?);
                    } else if key == "MAX_BYTES" {
                        max_bytes = Some(parse_bytes_to_u64(val)?);
                    }
                }
            }
            return Ok((before.to_string(), retention_ms, max_bytes));
        }
    }
    Ok((sql.to_string(), None, None))
}

fn validate_queue_where_clause(expr: &sqlparser::ast::Expr) -> H2Result<()> {
    match expr {
        sqlparser::ast::Expr::Identifier(ident) => {
            if !ident.value.eq_ignore_ascii_case("_offset") {
                return Err(H2Error::Unsupported(
                    "Only '_offset' column is allowed in WHERE clause on Queue Table".to_string(),
                ));
            }
        }
        sqlparser::ast::Expr::CompoundIdentifier(idents) => {
            if let Some(col) = idents.last() {
                if !col.value.eq_ignore_ascii_case("_offset") {
                    return Err(H2Error::Unsupported(
                        "Only '_offset' column is allowed in WHERE clause on Queue Table".to_string(),
                    ));
                }
            }
        }
        sqlparser::ast::Expr::BinaryOp { left, right, .. } => {
            validate_queue_where_clause(left)?;
            validate_queue_where_clause(right)?;
        }
        sqlparser::ast::Expr::UnaryOp { expr, .. } => {
            validate_queue_where_clause(expr)?;
        }
        sqlparser::ast::Expr::Between { expr, low, high, .. } => {
            validate_queue_where_clause(expr)?;
            validate_queue_where_clause(low)?;
            validate_queue_where_clause(high)?;
        }
        sqlparser::ast::Expr::Nested(e) => {
            validate_queue_where_clause(e)?;
        }
        sqlparser::ast::Expr::InList { expr, list, .. } => {
            validate_queue_where_clause(expr)?;
            for item in list {
                validate_queue_where_clause(item)?;
            }
        }
        sqlparser::ast::Expr::IsNull(e) | sqlparser::ast::Expr::IsNotNull(e) => {
            validate_queue_where_clause(e)?;
        }
        sqlparser::ast::Expr::Like { expr, pattern, .. }
        | sqlparser::ast::Expr::ILike { expr, pattern, .. } => {
            validate_queue_where_clause(expr)?;
            validate_queue_where_clause(pattern)?;
        }
        sqlparser::ast::Expr::Cast { expr, .. } => {
            validate_queue_where_clause(expr)?;
        }
        sqlparser::ast::Expr::Value(_) | sqlparser::ast::Expr::TypedString { .. } => {}
        _ => {}
    }
    Ok(())
}

fn extract_tables_from_query(query: &sqlparser::ast::Query) -> Vec<String> {
    let mut tables = Vec::new();
    if let sqlparser::ast::SetExpr::Select(ref select) = *query.body {
        for from_item in &select.from {
            extract_tables_from_table_factor(&from_item.relation, &mut tables);
            for join in &from_item.joins {
                extract_tables_from_table_factor(&join.relation, &mut tables);
            }
        }
    }
    tables
}

fn extract_tables_from_table_factor(tf: &sqlparser::ast::TableFactor, tables: &mut Vec<String>) {
    match tf {
        sqlparser::ast::TableFactor::Table { name, .. } => {
            let t = name.0.last().map(|i| i.value.clone()).unwrap_or_else(|| name.to_string());
            tables.push(t);
        }
        sqlparser::ast::TableFactor::Derived { subquery, .. } => {
            tables.extend(extract_tables_from_query(subquery));
        }
        _ => {}
    }
}
