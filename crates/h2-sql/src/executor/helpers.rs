use h2_types::{H2Error, H2Result, Value};

pub(crate) fn extract_file_path(param: &str) -> String {
    let p = param.trim().trim_end_matches(';').trim();
    p.trim_matches(|c| c == '\'' || c == '"' || c == '`').to_string()
}

pub(crate) fn parse_restore_pitr_options(opts: &str) -> H2Result<(Option<String>, h2_mvstore::RecoveryTarget)> {
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

pub(crate) fn split_sql_script(content: &str) -> Vec<String> {
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

pub(crate) fn format_csv_field(val: &Value, delimiter: char) -> String {
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

pub(crate) fn parse_csv_line(line: &str, delimiter: char) -> Vec<String> {
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

pub(crate) fn parse_duration_to_ms(s: &str) -> H2Result<u64> {
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

pub(crate) fn parse_bytes_to_u64(s: &str) -> H2Result<u64> {
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

pub(crate) fn parse_queue_with_clause(sql: &str) -> H2Result<(String, Option<u64>, Option<u64>)> {
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

#[derive(Debug, Clone)]
pub(crate) struct CacheTableOptions {
    pub ttl_ms: Option<u64>,
    pub write_back_table: Option<String>,
    pub write_back_interval_ms: Option<u64>,
    pub write_back_mode: Option<String>,
    pub is_unlogged: bool,
}

pub(crate) fn parse_cache_with_clause(sql: &str) -> H2Result<(String, CacheTableOptions)> {
    let upper = sql.to_uppercase();
    let mut opts = CacheTableOptions {
        ttl_ms: None,
        write_back_table: None,
        write_back_interval_ms: Some(1000),
        write_back_mode: Some("UPSERT".to_string()),
        is_unlogged: true,
    };

    if let Some(pos) = upper.rfind("WITH") {
        let before = sql[..pos].trim();
        let after = sql[pos + 4..].trim();
        if after.starts_with('(') && after.ends_with(')') {
            let inner = after[1..after.len() - 1].trim();
            for item in inner.split(',') {
                let kv: Vec<&str> = item.splitn(2, '=').collect();
                if kv.len() == 2 {
                    let key = kv[0].trim().to_ascii_uppercase();
                    let val = kv[1].trim().trim_matches('\'').trim_matches('"').trim();
                    match key.as_str() {
                        "TTL" | "RETENTION_TIME" => {
                            opts.ttl_ms = Some(parse_duration_to_ms(val)?);
                        }
                        "WRITE_BACK_TABLE" | "WRITE_BEHIND_TABLE" => {
                            opts.write_back_table = Some(val.to_string());
                        }
                        "WRITE_BACK_INTERVAL" | "WRITE_BEHIND_INTERVAL" | "FLUSH_INTERVAL" => {
                            opts.write_back_interval_ms = Some(parse_duration_to_ms(val)?);
                        }
                        "WRITE_BACK_MODE" | "WRITE_BEHIND_MODE" => {
                            opts.write_back_mode = Some(val.to_ascii_uppercase());
                        }
                        "UNLOGGED" => {
                            opts.is_unlogged = val.eq_ignore_ascii_case("TRUE") || val == "1";
                        }
                        _ => {}
                    }
                }
            }
            return Ok((before.to_string(), opts));
        }
    }
    Ok((sql.to_string(), opts))
}

pub(crate) fn validate_queue_where_clause(expr: &sqlparser::ast::Expr) -> H2Result<()> {
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

pub(crate) fn extract_tables_from_query(query: &sqlparser::ast::Query) -> Vec<String> {
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

pub(crate) fn extract_tables_from_table_factor(tf: &sqlparser::ast::TableFactor, tables: &mut Vec<String>) {
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

#[derive(Debug, Clone)]
pub(crate) struct RawPartitionSpec {
    pub name: String,
    pub source_col: String,
    pub transform: String,
}

fn split_top_level_commas(s: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut depth = 0;
    for c in s.chars() {
        match c {
            '(' | '[' => {
                depth += 1;
                current.push(c);
            }
            ')' | ']' => {
                if depth > 0 {
                    depth -= 1;
                }
                current.push(c);
            }
            ',' if depth == 0 => {
                let trimmed = current.trim().to_string();
                if !trimmed.is_empty() {
                    parts.push(trimmed);
                }
                current.clear();
            }
            _ => current.push(c),
        }
    }
    let trimmed = current.trim().to_string();
    if !trimmed.is_empty() {
        parts.push(trimmed);
    }
    parts
}

fn parse_single_partition_expr(raw_expr: &str) -> RawPartitionSpec {
    let s = raw_expr.trim();
    let s_lower = s.to_lowercase();

    if s_lower.starts_with("year(") && s.ends_with(')') {
        let col = s[5..s.len() - 1].trim().trim_matches('`').trim_matches('"');
        return RawPartitionSpec {
            name: format!("{}_year", col),
            source_col: col.to_string(),
            transform: "year".to_string(),
        };
    }
    if s_lower.starts_with("month(") && s.ends_with(')') {
        let col = s[6..s.len() - 1].trim().trim_matches('`').trim_matches('"');
        return RawPartitionSpec {
            name: format!("{}_month", col),
            source_col: col.to_string(),
            transform: "month".to_string(),
        };
    }
    if s_lower.starts_with("day(") && s.ends_with(')') {
        let col = s[4..s.len() - 1].trim().trim_matches('`').trim_matches('"');
        return RawPartitionSpec {
            name: format!("{}_day", col),
            source_col: col.to_string(),
            transform: "day".to_string(),
        };
    }
    if (s_lower.starts_with("bucket(") || s_lower.starts_with("bucket[")) && (s.ends_with(')') || s.ends_with(']')) {
        let inner = &s[7..s.len() - 1];
        let parts = split_top_level_commas(inner);
        if parts.len() == 2 {
            let (num, col) = if let Ok(n) = parts[0].parse::<u32>() {
                (n, parts[1].trim().trim_matches('`').trim_matches('"'))
            } else if let Ok(n) = parts[1].parse::<u32>() {
                (n, parts[0].trim().trim_matches('`').trim_matches('"'))
            } else {
                (16, parts[0].trim().trim_matches('`').trim_matches('"'))
            };
            return RawPartitionSpec {
                name: format!("{}_bucket", col),
                source_col: col.to_string(),
                transform: format!("bucket[{}]", num),
            };
        }
    }
    if (s_lower.starts_with("truncate(") || s_lower.starts_with("truncate[")) && (s.ends_with(')') || s.ends_with(']')) {
        let inner = &s[9..s.len() - 1];
        let parts = split_top_level_commas(inner);
        if parts.len() == 2 {
            let (w, col) = if let Ok(n) = parts[0].parse::<usize>() {
                (n, parts[1].trim().trim_matches('`').trim_matches('"'))
            } else if let Ok(n) = parts[1].parse::<usize>() {
                (n, parts[0].trim().trim_matches('`').trim_matches('"'))
            } else {
                (1, parts[0].trim().trim_matches('`').trim_matches('"'))
            };
            return RawPartitionSpec {
                name: format!("{}_trunc", col),
                source_col: col.to_string(),
                transform: format!("truncate[{}]", w),
            };
        }
    }
    if s_lower.starts_with("identity(") && s.ends_with(')') {
        let col = s[9..s.len() - 1].trim().trim_matches('`').trim_matches('"');
        return RawPartitionSpec {
            name: col.to_string(),
            source_col: col.to_string(),
            transform: "identity".to_string(),
        };
    }

    let col = s.trim_matches('`').trim_matches('"');
    RawPartitionSpec {
        name: col.to_string(),
        source_col: col.to_string(),
        transform: "identity".to_string(),
    }
}

pub(crate) fn parse_iceberg_ddl(sql: &str) -> H2Result<(String, String, Vec<RawPartitionSpec>)> {
    let mut location = None;
    let mut partition_specs = Vec::new();
    let trimmed = sql.trim().trim_end_matches(';').trim();
    let upper = trimmed.to_uppercase();

    // 1. Check WITH ( ... )
    if let Some(with_idx) = upper.rfind("WITH") {
        let with_body = trimmed[with_idx + 4..].trim();
        if with_body.starts_with('(') && with_body.ends_with(')') {
            let inner = &with_body[1..with_body.len() - 1];
            for part in inner.split(',') {
                let p = part.trim();
                if let Some(eq_idx) = p.find('=') {
                    let k = p[..eq_idx].trim().to_uppercase();
                    let v = p[eq_idx + 1..].trim().trim_matches('\'').trim_matches('"').trim();
                    if k == "LOCATION" {
                        location = Some(v.to_string());
                    } else if k == "PARTITION_BY" || k == "PARTITIONED_BY" {
                        for item in split_top_level_commas(v) {
                            partition_specs.push(parse_single_partition_expr(&item));
                        }
                    }
                }
            }
        }
    }

    // 2. Check LOCATION '...' keyword
    if location.is_none() {
        if let Some(loc_idx) = upper.rfind("LOCATION") {
            let after_loc = trimmed[loc_idx + 8..].trim();
            let after_loc = after_loc.strip_prefix('=').unwrap_or(after_loc).trim();
            if after_loc.starts_with('\'') || after_loc.starts_with('"') {
                let quote = after_loc.chars().next().unwrap();
                if let Some(end_q) = after_loc[1..].find(quote) {
                    location = Some(after_loc[1..=end_q].to_string());
                }
            } else {
                let token = after_loc.split_whitespace().next().unwrap_or(after_loc);
                location = Some(token.trim_end_matches(';').to_string());
            }
        }
    }

    // 3. Check PARTITIONED BY ( ... )
    if let Some(pos) = upper.find("PARTITIONED BY") {
        let after_p = &trimmed[pos + "PARTITIONED BY".len()..].trim();
        if after_p.starts_with('(') {
            let mut depth = 0;
            let mut end_idx = None;
            for (i, c) in after_p.char_indices() {
                if c == '(' {
                    depth += 1;
                } else if c == ')' {
                    depth -= 1;
                    if depth == 0 {
                        end_idx = Some(i);
                        break;
                    }
                }
            }
            if let Some(end_i) = end_idx {
                let inner = &after_p[1..end_i];
                for item in split_top_level_commas(inner) {
                    partition_specs.push(parse_single_partition_expr(&item));
                }
            }
        }
    }

    let loc = location.ok_or_else(|| {
        H2Error::Execution("Iceberg table requires a LOCATION clause (e.g. LOCATION '/path/to/table')".to_string())
    })?;

    // Clean SQL to standard CREATE TABLE
    let mut clean = trimmed.to_string();
    let clean_upper = clean.to_uppercase();
    if clean_upper.starts_with("CREATE EXTERNAL TABLE") {
        clean = format!("CREATE TABLE{}", &clean["CREATE EXTERNAL TABLE".len()..]);
    } else if clean_upper.starts_with("CREATE ICEBERG TABLE") {
        clean = format!("CREATE TABLE{}", &clean["CREATE ICEBERG TABLE".len()..]);
    }

    // Remove STORED AS ICEBERG / PARQUET
    if let Some(pos) = clean.to_uppercase().find("STORED AS ICEBERG") {
        clean = format!("{}{}", &clean[..pos], &clean[pos + "STORED AS ICEBERG".len()..]);
    }
    if let Some(pos) = clean.to_uppercase().find("STORED AS PARQUET") {
        clean = format!("{}{}", &clean[..pos], &clean[pos + "STORED AS PARQUET".len()..]);
    }

    // Remove PARTITIONED BY (...)
    if let Some(pos) = clean.to_uppercase().find("PARTITIONED BY") {
        let after = &clean[pos + "PARTITIONED BY".len()..];
        let trimmed_after = after.trim_start();
        let leading_spaces = after.len() - trimmed_after.len();
        if trimmed_after.starts_with('(') {
            let mut depth = 0;
            let mut end_idx = None;
            for (i, c) in trimmed_after.char_indices() {
                if c == '(' {
                    depth += 1;
                } else if c == ')' {
                    depth -= 1;
                    if depth == 0 {
                        end_idx = Some(i);
                        break;
                    }
                }
            }
            if let Some(end_i) = end_idx {
                let full_end = pos + "PARTITIONED BY".len() + leading_spaces + end_i + 1;
                clean = format!("{}{}", &clean[..pos], &clean[full_end..]);
            }
        }
    }

    // Remove WITH (...) if it was at the end (before stripping standalone LOCATION)
    if let Some(pos) = clean.to_uppercase().rfind("WITH") {
        let after = clean[pos + 4..].trim().trim_end_matches(';').trim();
        if after.starts_with('(') && after.ends_with(')') {
            clean = clean[..pos].trim().to_string();
        }
    }

    // Remove standalone LOCATION '...' if present
    if let Some(pos) = clean.to_uppercase().rfind("LOCATION") {
        clean = clean[..pos].trim().to_string();
    }

    clean = format!("{};", clean.trim().trim_end_matches(';').trim());
    Ok((clean, loc, partition_specs))
}
