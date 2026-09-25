use std::collections::HashMap;
use std::time::Duration;

use h2_types::query_metrics::QueryCounters;
use parking_lot::Mutex;
use sqlparser::dialect::PostgreSqlDialect;
use sqlparser::tokenizer::{Token, Tokenizer};

const MAX_QUERIES: usize = 256;

#[derive(Clone, Debug)]
pub struct QueryStat {
    pub query: String,
    pub calls: u64,
    pub errors: u64,
    pub rows: u64,
    pub total_ns: u64,
    pub min_ns: u64,
    pub max_ns: u64,
    pub counters: QueryCounters,
}

#[derive(Default)]
pub struct QueryStats {
    entries: Mutex<HashMap<String, QueryStat>>,
}

impl QueryStats {
    pub fn record(&self, sql: &str, elapsed: Duration, rows: u64, error: bool, counters: QueryCounters) {
        let query = normalize_query(sql);
        if query.is_empty() {
            return;
        }
        let mut entries = self.entries.lock();
        let key = if entries.contains_key(&query) || entries.len() < MAX_QUERIES - 1 {
            query
        } else {
            "[other queries]".to_string()
        };
        let elapsed_ns = elapsed.as_nanos().min(u64::MAX as u128) as u64;
        let entry = entries.entry(key.clone()).or_insert_with(|| QueryStat {
            query: key,
            calls: 0,
            errors: 0,
            rows: 0,
            total_ns: 0,
            min_ns: u64::MAX,
            max_ns: 0,
            counters: QueryCounters::default(),
        });
        entry.calls = entry.calls.saturating_add(1);
        entry.errors = entry.errors.saturating_add(u64::from(error));
        entry.rows = entry.rows.saturating_add(rows);
        entry.total_ns = entry.total_ns.saturating_add(elapsed_ns);
        entry.min_ns = entry.min_ns.min(elapsed_ns);
        entry.max_ns = entry.max_ns.max(elapsed_ns);
        let dst = &mut entry.counters;
        dst.lock_wait_ns = dst.lock_wait_ns.saturating_add(counters.lock_wait_ns);
        dst.tree_lock_wait_ns = dst.tree_lock_wait_ns.saturating_add(counters.tree_lock_wait_ns);
        dst.wal_lock_wait_ns = dst.wal_lock_wait_ns.saturating_add(counters.wal_lock_wait_ns);
        dst.commit_lock_wait_ns = dst.commit_lock_wait_ns.saturating_add(counters.commit_lock_wait_ns);
        dst.wal_write_ns = dst.wal_write_ns.saturating_add(counters.wal_write_ns);
        dst.wal_sync_ns = dst.wal_sync_ns.saturating_add(counters.wal_sync_ns);
        dst.wal_durable_wait_ns = dst.wal_durable_wait_ns.saturating_add(counters.wal_durable_wait_ns);
        dst.point_gets = dst.point_gets.saturating_add(counters.point_gets);
        dst.scans = dst.scans.saturating_add(counters.scans);
        dst.scan_entries = dst.scan_entries.saturating_add(counters.scan_entries);
    }

    pub fn snapshot(&self) -> Vec<QueryStat> {
        let mut entries: Vec<_> = self.entries.lock().values().cloned().collect();
        entries.sort_by(|a, b| b.total_ns.cmp(&a.total_ns).then_with(|| a.query.cmp(&b.query)));
        entries
    }

    pub fn reset(&self) {
        self.entries.lock().clear();
    }
}

/// Group literal variants together without retaining string contents.
pub fn normalize_query(sql: &str) -> String {
    let dialect = PostgreSqlDialect {};
    let Ok(tokens) = Tokenizer::new(&dialect, sql).tokenize() else {
        return "[unparseable query]".to_string();
    };
    let mut parts = Vec::new();
    for token in tokens {
        let part = match token {
            Token::Whitespace(_) | Token::EOF | Token::SemiColon => continue,
            Token::Number(..)
            | Token::SingleQuotedString(..)
            | Token::DoubleQuotedString(..)
            | Token::DollarQuotedString(..)
            | Token::EscapedStringLiteral(..)
            | Token::NationalStringLiteral(..)
            | Token::HexStringLiteral(..) => "?".to_string(),
            Token::Word(word) if word.quote_style.is_none() => word.value.to_uppercase(),
            other => other.to_string(),
        };
        parts.push(part);
    }
    parts.join(" ").chars().take(512).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_literals_without_retaining_string_values() {
        assert_eq!(normalize_query("SELECT * FROM t WHERE id = 1 AND name = 'secret'"),
                   normalize_query("select * from t where id=2 and name='other'"));
        assert!(!normalize_query("SELECT 'secret'").contains("secret"));
    }

    #[test]
    fn aggregates_calls_and_waits() {
        let stats = QueryStats::default();
        stats.record("SELECT 1", Duration::from_millis(2), 1, false,
                     QueryCounters { lock_wait_ns: 1_000_000, ..Default::default() });
        stats.record("select 2", Duration::from_millis(3), 1, true,
                     QueryCounters { lock_wait_ns: 2_000_000, ..Default::default() });
        let entries = stats.snapshot();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].calls, 2);
        assert_eq!(entries[0].errors, 1);
        assert_eq!(entries[0].counters.lock_wait_ns, 3_000_000);
    }
}
