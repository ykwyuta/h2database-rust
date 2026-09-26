use sqlparser::ast::Statement;
use h2_mvstore::Transaction;
use h2_types::{H2Error, H2Result, Value};
use crate::catalog::{IndexDef, TableDef};
use crate::parser::{extract_create_table, parse_sql};
use crate::row::Row;
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

}
