use std::str::FromStr;
use sqlparser::ast::{
    Statement, TableFactor,
};
use h2_types::{H2Error, H2Result, Value};
use crate::row::Row;
use super::*;

impl SQLEngine {
    pub(crate) fn require_stats_admin(&self, user: Option<&str>) -> H2Result<()> {
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

    pub(crate) fn show_query_stats(&self) -> ExecutionResult {
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

    pub(crate) fn execute_create_user(&self, sql: &str) -> H2Result<ExecutionResult> {
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

    pub(crate) fn execute_alter_user(&self, sql: &str) -> H2Result<ExecutionResult> {
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

    pub(crate) fn execute_drop_user(&self, sql: &str) -> H2Result<ExecutionResult> {
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

    pub(crate) fn execute_grant(&self, sql: &str) -> H2Result<ExecutionResult> {
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

    pub(crate) fn execute_revoke(&self, sql: &str) -> H2Result<ExecutionResult> {
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

    pub(crate) fn execute_show_users(&self) -> H2Result<ExecutionResult> {
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

    pub(crate) fn execute_show_grants(&self, user: &str) -> H2Result<ExecutionResult> {
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

    pub(crate) fn check_statement_privileges(&self, user: &str, stmt: &Statement) -> H2Result<()> {
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
