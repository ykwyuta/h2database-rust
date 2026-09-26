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
