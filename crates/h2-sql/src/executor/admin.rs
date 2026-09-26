use sqlparser::ast::{SetExpr, Statement};
use h2_mvstore::Transaction;
use h2_types::{H2Error, H2Result, Value};
use crate::catalog::{IndexDef, TableDef};
use crate::expression::{evaluate_expr_context, RowContext};
use crate::parser::{extract_create_table, parse_sql};
use crate::row::Row;
use super::encoding::*;
use super::*;

impl SQLEngine {
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

    pub(crate) fn execute_alter_sequence(&self, sql: &str) -> H2Result<ExecutionResult> {
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

    pub(crate) fn execute_create_queue_table(&self, _tx: &Transaction, sql: &str) -> H2Result<ExecutionResult> {
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

    pub(crate) fn execute_vacuum(&self, sql: &str) -> H2Result<ExecutionResult> {
        let parts: Vec<&str> = sql.split_whitespace().collect();
        if parts.len() >= 2 && parts[1].eq_ignore_ascii_case("FULL") {
            self.store.compact()?;
            return Ok(ExecutionResult::Ddl);
        }

        if parts.len() >= 2 && !parts[1].eq_ignore_ascii_case("FULL") {
            let table_name = parts[1].trim_matches(';').trim_matches('"').trim_matches('\'');
            let table_def = self.catalog.get_table(table_name)
                .ok_or_else(|| H2Error::Catalog(format!("Table '{}' not found", table_name)))?;
            let map_name = table_def.map_name();
            self.tx_store.vacuum_map(&map_name)?;
        } else {
            for table in self.catalog.all_tables() {
                let map_name = table.map_name();
                self.tx_store.vacuum_map(&map_name)?;
            }
        }
        Ok(ExecutionResult::Ddl)
    }

    pub(crate) fn execute_analyze(&self, tx: &Transaction, sql: &str) -> H2Result<ExecutionResult> {
        let parts: Vec<&str> = sql.split_whitespace().collect();
        let mut sample_spec = crate::stats::SampleSpec::Default;
        let mut table_name_opt = None;

        let mut i = 1;
        while i < parts.len() {
            let part_upper = parts[i].to_uppercase();
            let clean_part = parts[i].trim_matches(';').trim_matches('"').trim_matches('\'');
            if part_upper == "WITH" {
                i += 1;
                continue;
            }
            if part_upper == "SAMPLE" {
                i += 1;
                if i < parts.len() {
                    let num_str = parts[i].trim_matches(';').trim_matches('"').trim_matches('\'').trim_end_matches('%');
                    let is_pct_sign = parts[i].ends_with('%');
                    i += 1;
                    let mut is_percent = is_pct_sign;
                    if i < parts.len() {
                        let unit = parts[i].to_uppercase();
                        if unit.starts_with("PERCENT") {
                            is_percent = true;
                            i += 1;
                        } else if unit.starts_with("ROW") {
                            is_percent = false;
                            i += 1;
                        }
                    }
                    if is_percent {
                        let pct = num_str.parse::<f64>().map_err(|_| {
                            H2Error::Execution(format!("Invalid sample percent: {}", num_str))
                        })?;
                        sample_spec = crate::stats::SampleSpec::Percent(pct);
                    } else {
                        let rows = num_str.parse::<usize>().map_err(|_| {
                            H2Error::Execution(format!("Invalid sample rows: {}", num_str))
                        })?;
                        sample_spec = crate::stats::SampleSpec::Rows(rows);
                    }
                }
            } else if table_name_opt.is_none() {
                table_name_opt = Some(clean_part);
                i += 1;
            } else {
                i += 1;
            }
        }

        if let Some(table_name) = table_name_opt {
            let table_def = self.catalog.get_table(table_name)
                .ok_or_else(|| H2Error::Catalog(format!("Table '{}' not found", table_name)))?;
            let (stats, col_stats) = crate::stats::analyze_table_with_sample(tx, &table_def, sample_spec)?;
            self.catalog.update_table_stats(table_name, stats, col_stats)?;
        } else {
            let tables = self.catalog.all_tables();
            for table_def in tables {
                let (stats, col_stats) = crate::stats::analyze_table_with_sample(tx, &table_def, sample_spec)?;
                self.catalog.update_table_stats(&table_def.name, stats, col_stats)?;
            }
        }
        self.store.commit()?;
        Ok(ExecutionResult::Ddl)
    }

    pub(crate) fn execute_show_stats(&self, sql: &str) -> H2Result<ExecutionResult> {
        let parts: Vec<&str> = sql.split_whitespace().collect();
        let table_name_opt = if parts.len() >= 4 && parts[2].eq_ignore_ascii_case("FOR") {
            Some(parts[3].trim_matches(';').trim_matches('"').trim_matches('\''))
        } else if parts.len() >= 3 && !parts[2].eq_ignore_ascii_case("FOR") {
            Some(parts[2].trim_matches(';').trim_matches('"').trim_matches('\''))
        } else {
            None
        };

        if let Some(table_name) = table_name_opt {
            let table_def = self.catalog.get_table(table_name)
                .ok_or_else(|| H2Error::Catalog(format!("Table '{}' not found", table_name)))?;
            let columns = vec![
                "COLUMN_NAME".to_string(),
                "DATA_TYPE".to_string(),
                "NDV".to_string(),
                "NULL_FRAC".to_string(),
                "AVG_WIDTH".to_string(),
                "MCV".to_string(),
            ];
            let mut rows = Vec::new();
            for col in &table_def.columns {
                if let Some(ref cs) = col.stats {
                    let mcv_str = cs.most_common_vals.iter()
                        .take(5)
                        .map(|v| format!("{:?}", v))
                        .collect::<Vec<_>>()
                        .join(", ");
                    rows.push(Row::new(vec![
                        Value::String(col.name.clone()),
                        Value::String(col.data_type.to_string()),
                        Value::BigInt(cs.ndv as i64),
                        Value::Double(cs.null_frac),
                        Value::Double(cs.avg_width),
                        Value::String(mcv_str),
                    ]));
                } else {
                    rows.push(Row::new(vec![
                        Value::String(col.name.clone()),
                        Value::String(col.data_type.to_string()),
                        Value::Null,
                        Value::Null,
                        Value::Null,
                        Value::String(String::new()),
                    ]));
                }
            }
            Ok(ExecutionResult::Query { columns, rows })
        } else {
            let columns = vec![
                "TABLE_NAME".to_string(),
                "ROW_COUNT".to_string(),
                "TOTAL_PAGES".to_string(),
                "SAMPLE_RATIO".to_string(),
                "LAST_ANALYZED".to_string(),
            ];
            let mut rows = Vec::new();
            for table in self.catalog.all_tables() {
                if let Some(ref s) = table.stats {
                    rows.push(Row::new(vec![
                        Value::String(table.name.clone()),
                        Value::BigInt(s.row_count as i64),
                        Value::BigInt(s.total_pages as i64),
                        Value::Double(s.sample_ratio),
                        s.last_analyzed.map(|ms| Value::BigInt(ms as i64)).unwrap_or(Value::Null),
                    ]));
                } else {
                    rows.push(Row::new(vec![
                        Value::String(table.name.clone()),
                        Value::BigInt(table.approx_row_count as i64),
                        Value::Null,
                        Value::Null,
                        Value::Null,
                    ]));
                }
            }
            Ok(ExecutionResult::Query { columns, rows })
        }
    }

    pub(crate) fn execute_set_max_materialized_rows(&self, sql: &str) -> H2Result<ExecutionResult> {
        let val_part = sql.split('=').nth(1)
            .or_else(|| sql.split_whitespace().nth(2))
            .ok_or_else(|| H2Error::Execution("Expected value in SET max_materialized_rows = <num>".to_string()))?;
        let trimmed_val = val_part.trim().trim_matches(';').trim_matches('\'').trim_matches('"');
        let rows = trimmed_val.parse::<usize>()
            .map_err(|_| H2Error::Execution(format!("Invalid integer for max_materialized_rows: {}", trimmed_val)))?;
        self.memory_config.set_max_materialized_rows(rows);
        Ok(ExecutionResult::Ddl)
    }

    pub(crate) fn execute_set_work_mem(&self, sql: &str) -> H2Result<ExecutionResult> {
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

    pub(crate) fn execute_set_execution_mode(&self, sql: &str) -> H2Result<ExecutionResult> {
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

    pub(crate) fn execute_create_cache_table(&self, _tx: &Transaction, sql: &str) -> H2Result<ExecutionResult> {
        if self.is_read_only() {
            return Err(H2Error::ReadOnly(
                "Cannot execute data-modifying or DDL statements on a read-only instance".to_string(),
            ));
        }

        let (cleaned_sql, cache_opts) = parse_cache_with_clause(sql)?;

        let regex_cct = regex::Regex::new(r"(?i)^CREATE\s+(CACHE|MEMORY)\s+TABLE").unwrap();
        let create_table_sql = regex_cct.replace(&cleaned_sql, "CREATE TABLE").to_string();

        let statements = parse_sql(&create_table_sql)?;
        let stmt = statements.into_iter().next().ok_or_else(|| {
            H2Error::Execution("Failed to parse CREATE CACHE TABLE".to_string())
        })?;

        match stmt {
            Statement::CreateTable(create_table) => {
                let table_def = extract_create_table(
                    &create_table.name,
                    &create_table.columns,
                    &create_table.constraints,
                )?;
                let tbl_name = table_def.name.clone();
                let cache_def = TableDef::new_cache(
                    tbl_name.clone(),
                    table_def.columns,
                    cache_opts.ttl_ms,
                    cache_opts.write_back_table,
                    cache_opts.write_back_interval_ms,
                    cache_opts.write_back_mode,
                    cache_opts.is_unlogged,
                );

                if let Err(e) = self.catalog.create_table(cache_def) {
                    if create_table.if_not_exists && e.to_string().contains("already exists") {
                        return Ok(ExecutionResult::Ddl);
                    }
                    return Err(e);
                }

                if !table_def.primary_key.is_empty() {
                    let pk_index_name = format!("pk_{}", tbl_name.to_lowercase());
                    let _ = self.catalog.create_index(IndexDef {
                        name: pk_index_name,
                        table_name: tbl_name.clone(),
                        columns: table_def.primary_key.clone(),
                        is_unique: true,
                    });
                }

                self.store.commit()?;
                Ok(ExecutionResult::Ddl)
            }
            _ => Err(H2Error::Execution("Expected CREATE TABLE statement".to_string())),
        }
    }

    pub(crate) fn execute_create_iceberg_table(&self, _tx: &Transaction, sql: &str) -> H2Result<ExecutionResult> {
        let (clean_sql, location) = parse_iceberg_ddl(sql)?;
        let parsed = parse_sql(&clean_sql)?;
        match parsed.into_iter().next() {
            Some(Statement::CreateTable(create_table)) => {
                let table_def = extract_create_table(
                    &create_table.name,
                    &create_table.columns,
                    &create_table.constraints,
                )?;
                let tbl_name = table_def.name.clone();
                let mut final_table_def = table_def;
                if final_table_def.columns.is_empty() {
                    if let Ok((_ver, meta)) = crate::iceberg::get_latest_metadata(&location) {
                        if let Some(schema) = meta.schemas.iter().find(|s| s.schema_id == meta.current_schema_id) {
                            for f in &schema.fields {
                                final_table_def.columns.push(crate::catalog::ColumnDef::new(
                                    f.name.clone(),
                                    crate::iceberg::metadata::iceberg_type_to_h2_type(&f.type_name),
                                    !f.required,
                                    false,
                                ));
                            }
                        }
                    }
                }

                crate::iceberg::init_iceberg_table(&location, &final_table_def)?;

                let iceberg_def = TableDef::new_iceberg(
                    tbl_name,
                    final_table_def.columns,
                    location,
                );

                if let Err(e) = self.catalog.create_table(iceberg_def) {
                    if create_table.if_not_exists && e.to_string().contains("already exists") {
                        return Ok(ExecutionResult::Ddl);
                    }
                    return Err(e);
                }

                self.store.commit()?;
                Ok(ExecutionResult::Ddl)
            }
            _ => Err(H2Error::Execution("Expected CREATE TABLE statement for Iceberg".to_string())),
        }
    }

    pub(crate) fn execute_touch(&self, tx: &Transaction, sql: &str) -> H2Result<ExecutionResult> {
        let trimmed = sql.trim().trim_end_matches(';').trim();
        let upper = trimmed.to_uppercase();
        let after_touch = if upper.starts_with("TOUCH TABLE ") {
            trimmed[12..].trim()
        } else if upper.starts_with("TOUCH ") {
            trimmed[6..].trim()
        } else {
            return Err(H2Error::Execution("Invalid TOUCH syntax".to_string()));
        };

        let upper_after = after_touch.to_uppercase();
        let where_pos = upper_after.find("WHERE").ok_or_else(|| {
            H2Error::Execution("TOUCH requires a WHERE clause".to_string())
        })?;

        let tbl_name = after_touch[..where_pos].trim().trim_matches('"').trim_matches('\'');
        let mut where_and_extend = after_touch[where_pos + 5..].trim();

        let upper_we = where_and_extend.to_uppercase();
        let mut extend_ms = None;
        if let Some(ext_pos) = upper_we.rfind("EXTEND") {
            let dur_str = where_and_extend[ext_pos + 6..].trim();
            extend_ms = Some(parse_duration_to_ms(dur_str)?);
            where_and_extend = where_and_extend[..ext_pos].trim();
        }

        let table_def = self.catalog.get_table(tbl_name).ok_or_else(|| {
            H2Error::Catalog(format!("Table '{}' not found", tbl_name))
        })?;

        if !table_def.is_cache {
            return Err(H2Error::Execution(format!("Table '{}' is not a cache table", tbl_name)));
        }

        let extend_duration = extend_ms.or(table_def.cache_ttl_ms).unwrap_or(3600_000);

        let dummy_query = format!("SELECT * FROM dummy WHERE {}", where_and_extend);
        let parsed = parse_sql(&dummy_query)?;
        let where_expr = match parsed.into_iter().next() {
            Some(Statement::Query(q)) => match *q.body {
                SetExpr::Select(s) => s.selection.ok_or_else(|| H2Error::Execution("Empty WHERE clause".to_string()))?,
                _ => return Err(H2Error::Execution("Invalid WHERE condition".to_string())),
            },
            _ => return Err(H2Error::Execution("Invalid WHERE condition".to_string())),
        };

        let ctx = RowContext::from_table_def(&table_def, None);
        let map_name = table_def.map_name();
        let entries = tx.scan_visible(&map_name)?;
        let now_ms = chrono::Utc::now().timestamp_millis();
        let mut affected = 0;

        for (k, val_bytes) in entries {
            let mut row = Row::from_bytes(&val_bytes)?;
            table_def.align_row(&mut row);

            let old_exp = match row.values.get(0) {
                Some(Value::BigInt(e)) => *e,
                _ => continue,
            };
            if old_exp <= now_ms {
                continue;
            }

            let is_match = match evaluate_expr_context(&where_expr, &ctx, &row)? {
                Value::Boolean(b) => b,
                _ => false,
            };

            if is_match {
                let base = if old_exp > now_ms { old_exp } else { now_ms };
                row.values[0] = Value::BigInt(base + extend_duration as i64);
                row.values[2] = Value::Boolean(true);
                tx.put(&map_name, k, row.to_bytes()?)?;
                affected += 1;
            }
        }

        Ok(ExecutionResult::Dml { affected_rows: affected })
    }

    pub(crate) fn execute_flush_write_back(&self, tx: &Transaction, sql: &str) -> H2Result<ExecutionResult> {
        let parts: Vec<&str> = sql.trim().trim_end_matches(';').split_whitespace().collect();
        if parts.len() < 2 {
            return Err(H2Error::Execution("Expected FLUSH WRITE_BACK <table_name>".to_string()));
        }
        let tbl_name = parts.last().unwrap().trim_matches('"').trim_matches('\'');
        let count = self.flush_cache_write_back(tx, tbl_name)?;
        Ok(ExecutionResult::Dml { affected_rows: count as u64 })
    }

    pub(crate) fn execute_purge_expired(&self, tx: &Transaction, sql: &str) -> H2Result<ExecutionResult> {
        let parts: Vec<&str> = sql.trim().trim_end_matches(';').split_whitespace().collect();
        if parts.len() < 2 {
            return Err(H2Error::Execution("Expected PURGE EXPIRED FROM <table_name>".to_string()));
        }
        let tbl_name = parts.last().unwrap().trim_matches('"').trim_matches('\'');
        let count = self.purge_cache_expired(tx, tbl_name)?;
        Ok(ExecutionResult::Dml { affected_rows: count as u64 })
    }

    pub fn flush_cache_write_back(&self, tx: &Transaction, table_name: &str) -> H2Result<usize> {
        let table_def = self.catalog.get_table(table_name).ok_or_else(|| {
            H2Error::Catalog(format!("Table '{}' not found", table_name))
        })?;

        if !table_def.is_cache {
            return Err(H2Error::Execution(format!("Table '{}' is not a cache table", table_name)));
        }

        let target_tbl_name = match &table_def.write_back_table {
            Some(t) => t.clone(),
            None => return Ok(0),
        };

        let target_table_def = self.catalog.get_table(&target_tbl_name).ok_or_else(|| {
            H2Error::Catalog(format!("Write-back target table '{}' not found", target_tbl_name))
        })?;

        let mode = table_def.write_back_mode.as_deref().unwrap_or("UPSERT");
        let now_ms = chrono::Utc::now().timestamp_millis();
        let map_name = table_def.map_name();
        let entries = tx.scan_visible(&map_name)?;
        let mut count = 0;

        let mut col_map = Vec::new();
        for (cache_idx, col) in table_def.columns.iter().enumerate().skip(3) {
            if let Some(target_idx) = target_table_def.column_index(&col.name) {
                col_map.push((cache_idx, target_idx));
            }
        }

        let pk_cols = &target_table_def.primary_key;

        for (k, val_bytes) in entries {
            let mut row = Row::from_bytes(&val_bytes)?;
            table_def.align_row(&mut row);

            let expires_at = match row.values.get(0) {
                Some(Value::BigInt(e)) => *e,
                _ => i64::MAX,
            };
            let is_expired = expires_at <= now_ms;
            let is_dirty = match row.values.get(2) {
                Some(Value::Boolean(b)) => *b,
                _ => false,
            };

            if is_expired {
                if mode.eq_ignore_ascii_case("DELETE") || mode.eq_ignore_ascii_case("BOTH") {
                    self.delete_matching_target_row(tx, &target_table_def, &table_def, &row)?;
                }
                tx.remove(&map_name, &k)?;
                count += 1;
            } else if is_dirty {
                self.upsert_into_target_table(tx, &target_table_def, &table_def, &row, &col_map, pk_cols)?;
                row.values[2] = Value::Boolean(false);
                tx.put(&map_name, k, row.to_bytes()?)?;
                count += 1;
            }
        }

        Ok(count)
    }

    pub fn purge_cache_expired(&self, tx: &Transaction, table_name: &str) -> H2Result<usize> {
        let table_def = self.catalog.get_table(table_name).ok_or_else(|| {
            H2Error::Catalog(format!("Table '{}' not found", table_name))
        })?;

        if !table_def.is_cache {
            return Err(H2Error::Execution(format!("Table '{}' is not a cache table", table_name)));
        }

        let now_ms = chrono::Utc::now().timestamp_millis();
        let map_name = table_def.map_name();
        let entries = tx.scan_visible(&map_name)?;
        let mut purged = 0;

        let target_tbl = table_def.write_back_table.as_ref().and_then(|t| self.catalog.get_table(t));
        let mode = table_def.write_back_mode.as_deref().unwrap_or("UPSERT");
        let should_delete_backing = mode.eq_ignore_ascii_case("DELETE") || mode.eq_ignore_ascii_case("BOTH");

        for (k, val_bytes) in entries {
            let mut row = Row::from_bytes(&val_bytes)?;
            table_def.align_row(&mut row);

            let expires_at = match row.values.get(0) {
                Some(Value::BigInt(e)) => *e,
                _ => continue,
            };
            if expires_at <= now_ms {
                if should_delete_backing {
                    if let Some(ref target_def) = target_tbl {
                        let _ = self.delete_matching_target_row(tx, target_def, &table_def, &row);
                    }
                }
                tx.remove(&map_name, &k)?;
                purged += 1;
            }
        }

        Ok(purged)
    }

    fn delete_matching_target_row(
        &self,
        tx: &Transaction,
        target_def: &TableDef,
        cache_def: &TableDef,
        cache_row: &Row,
    ) -> H2Result<bool> {
        let pk_cols = &target_def.primary_key;
        if pk_cols.is_empty() {
            return Ok(false);
        }
        let target_map_name = target_def.map_name();
        let target_entries = tx.scan_visible(&target_map_name)?;
        let mut target_rid = None;
        let mut matched_target_row = None;

        for (tk, t_bytes) in target_entries {
            let mut t_row = Row::from_bytes(&t_bytes)?;
            target_def.align_row(&mut t_row);
            let matches_pk = pk_cols.iter().all(|pk| {
                if let (Some(ci), Some(ti)) = (cache_def.column_index(pk), target_def.column_index(pk)) {
                    cache_row.get(ci) == t_row.get(ti)
                } else {
                    false
                }
            });
            if matches_pk {
                if tk.len() == 8 {
                    let rid = u64::from_le_bytes(tk.as_slice().try_into().unwrap());
                    target_rid = Some(rid);
                    matched_target_row = Some(t_row);
                    break;
                }
            }
        }

        if let (Some(rid), Some(t_row)) = (target_rid, matched_target_row) {
            let indexes = self.catalog.get_table_indexes(&target_def.name);
            for idx in &indexes {
                if let Some(vals) = get_index_values(target_def, idx, &t_row) {
                    let idx_map = format!("idx_{}_{}", target_def.name.to_lowercase(), idx.name.to_lowercase());
                    let idx_k = encode_composite_index_key(&vals, rid);
                    let _ = tx.remove(&idx_map, &idx_k);
                }
            }
            tx.remove(&target_map_name, &rid.to_le_bytes())?;
            let _ = self.catalog.update_approx_row_count(&target_def.name, -1);
            return Ok(true);
        }
        Ok(false)
    }

    fn upsert_into_target_table(
        &self,
        tx: &Transaction,
        target_def: &TableDef,
        cache_def: &TableDef,
        cache_row: &Row,
        col_map: &[(usize, usize)],
        pk_cols: &[String],
    ) -> H2Result<()> {
        let target_map_name = target_def.map_name();
        let target_entries = tx.scan_visible(&target_map_name)?;
        let mut target_rid = None;
        let mut existing_target_row = None;

        if !pk_cols.is_empty() {
            for (tk, t_bytes) in target_entries {
                let mut t_row = Row::from_bytes(&t_bytes)?;
                target_def.align_row(&mut t_row);
                let matches_pk = pk_cols.iter().all(|pk| {
                    if let (Some(ci), Some(ti)) = (cache_def.column_index(pk), target_def.column_index(pk)) {
                        cache_row.get(ci) == t_row.get(ti)
                    } else {
                        false
                    }
                });
                if matches_pk {
                    if tk.len() == 8 {
                        let rid = u64::from_le_bytes(tk.as_slice().try_into().unwrap());
                        target_rid = Some(rid);
                        existing_target_row = Some(t_row);
                        break;
                    }
                }
            }
        }

        let indexes = self.catalog.get_table_indexes(&target_def.name);

        if let (Some(rid), Some(mut old_target_row)) = (target_rid, existing_target_row) {
            let old_row_copy = old_target_row.clone();
            for &(ci, ti) in col_map {
                if let Some(val) = cache_row.get(ci) {
                    old_target_row.values[ti] = val.clone();
                }
            }

            for idx in &indexes {
                if let (Some(old_vals), Some(new_vals)) = (
                    get_index_values(target_def, idx, &old_row_copy),
                    get_index_values(target_def, idx, &old_target_row),
                ) {
                    if old_vals != new_vals {
                        let idx_map = format!("idx_{}_{}", target_def.name.to_lowercase(), idx.name.to_lowercase());
                        let old_k = encode_composite_index_key(&old_vals, rid);
                        let _ = tx.remove(&idx_map, &old_k);
                        let new_k = encode_composite_index_key(&new_vals, rid);
                        tx.put(&idx_map, new_k, vec![])?;
                    }
                }
            }
            tx.put(&target_map_name, rid.to_le_bytes().to_vec(), old_target_row.to_bytes()?)?;
        } else {
            let new_rid = self.catalog.allocate_row_id(&target_def.name)?;
            let mut new_vals = vec![Value::Null; target_def.columns.len()];
            for &(ci, ti) in col_map {
                if let Some(val) = cache_row.get(ci) {
                    new_vals[ti] = val.clone();
                }
            }
            let new_row = Row::new(new_vals);
            for idx in &indexes {
                if let Some(vals) = get_index_values(target_def, idx, &new_row) {
                    let idx_map = format!("idx_{}_{}", target_def.name.to_lowercase(), idx.name.to_lowercase());
                    let new_k = encode_composite_index_key(&vals, new_rid);
                    tx.put(&idx_map, new_k, vec![])?;
                }
            }
            tx.put(&target_map_name, new_rid.to_le_bytes().to_vec(), new_row.to_bytes()?)?;
            let _ = self.catalog.update_approx_row_count(&target_def.name, 1);
        }

        Ok(())
    }
}
