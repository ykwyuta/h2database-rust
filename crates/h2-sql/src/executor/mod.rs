pub mod admin;
pub mod dcl;
pub mod encoding;
pub mod foreign_key;
pub mod helpers;
pub mod query;
pub mod statement;
pub mod table_factor;
pub mod window;

pub(crate) use self::encoding::*;
pub(crate) use self::helpers::*;
pub(crate) use self::window::*;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use sqlparser::ast::{Expr, SelectItem, Statement};
use h2_mvstore::{MVStore, Transaction, TransactionStore};
use h2_types::{H2Error, H2Result, Value};
use h2_types::query_metrics::QueryMetricsGuard;
use crate::catalog::{Catalog, TableDef};
use crate::expression::{evaluate_expr_context, RowContext};
use crate::row::Row;

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
    autovacuum: Arc<crate::autovacuum::AutoVacuumCoordinator>,
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
        let autovacuum = Arc::new(crate::autovacuum::AutoVacuumCoordinator::new(
            crate::autovacuum::AutoVacuumConfig::default(),
        ));
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
            autovacuum,
        })
    }

    pub fn autovacuum(&self) -> &Arc<crate::autovacuum::AutoVacuumCoordinator> {
        &self.autovacuum
    }

    pub fn run_autovacuum_if_needed(&self) -> H2Result<Vec<(String, bool, bool)>> {
        self.autovacuum.run_all_maintenance(&self.tx_store, &self.catalog)
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
        let trimmed_upper = trimmed.to_uppercase();
        let result = if trimmed_upper == "VACUUM FULL" {
            self.store.compact().map(|_| ExecutionResult::Ddl)
        } else if trimmed_upper == "VACUUM" || trimmed_upper.starts_with("VACUUM ") {
            self.execute_vacuum(trimmed)
        } else {
            let tx = self.tx_store.begin();
            let result = self.execute_with_user_and_tx_inner(&tx, sql, user);
            match result {
                Ok(value) => {
                    let commit_res = tx.commit();
                    if commit_res.is_ok() && matches!(value, ExecutionResult::Dml { .. }) {
                        let _ = self.autovacuum.run_all_maintenance(&self.tx_store, &self.catalog);
                    }
                    commit_res.map(|_| value)
                }
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
            if trimmed_upper == "VACUUM"
                || trimmed_upper.starts_with("VACUUM ")
                || trimmed_upper == "VACUUM FULL"
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

        if trimmed_upper == "VACUUM FULL" {
            self.store.compact()?;
            return Ok(ExecutionResult::Ddl);
        }

        if trimmed_upper == "VACUUM" || trimmed_upper.starts_with("VACUUM ") {
            return self.execute_vacuum(trimmed);
        }

        if trimmed_upper == "AUTOVACUUM" || trimmed_upper == "AUTOVACUUM RUN" || trimmed_upper == "MAINTENANCE" {
            let res = self.autovacuum.run_all_maintenance(&self.tx_store, &self.catalog)?;
            let columns = vec!["table_name".to_string(), "vacuumed".to_string(), "analyzed".to_string()];
            let rows = res.into_iter().map(|(tbl, vac, ana)| {
                crate::row::Row::new(vec![
                    Value::String(tbl),
                    Value::Boolean(vac),
                    Value::Boolean(ana),
                ])
            }).collect();
            return Ok(ExecutionResult::Query { columns, rows });
        }

        if trimmed_upper == "SHOW STATS" || trimmed_upper.starts_with("SHOW STATS ") {
            return self.execute_show_stats(trimmed);
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

        if trimmed_upper.starts_with("CREATE EXTERNAL TABLE")
            || trimmed_upper.starts_with("CREATE ICEBERG TABLE")
            || (trimmed_upper.starts_with("CREATE TABLE")
                && (trimmed_upper.contains("STORED AS ICEBERG")
                    || trimmed_upper.contains("STORED AS PARQUET")
                    || trimmed_upper.contains("TYPE = 'ICEBERG'")
                    || trimmed_upper.contains("TYPE='ICEBERG'")))
        {
            return self.execute_create_iceberg_table(tx, trimmed);
        }

        if trimmed_upper.starts_with("CREATE CACHE TABLE")
            || trimmed_upper.starts_with("CREATE MEMORY TABLE")
            || (trimmed_upper.starts_with("CREATE TABLE")
                && (trimmed_upper.contains("TYPE = 'CACHE'")
                    || trimmed_upper.contains("TYPE='CACHE'")
                    || trimmed_upper.contains("TYPE = 'MEMORY'")
                    || trimmed_upper.contains("TYPE='MEMORY'")))
        {
            return self.execute_create_cache_table(tx, trimmed);
        }

        if trimmed_upper.starts_with("TOUCH ") || trimmed_upper.starts_with("TOUCH TABLE ") {
            return self.execute_touch(tx, trimmed);
        }

        if trimmed_upper.starts_with("FLUSH WRITE_BACK")
            || trimmed_upper.starts_with("FLUSH CACHE")
            || trimmed_upper.starts_with("FLUSH WRITE_BEHIND")
        {
            return self.execute_flush_write_back(tx, trimmed);
        }

        if trimmed_upper.starts_with("PURGE EXPIRED") {
            return self.execute_purge_expired(tx, trimmed);
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

}
