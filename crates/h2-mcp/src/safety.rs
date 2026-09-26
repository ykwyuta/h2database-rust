use std::time::Duration;
use h2_types::{H2Error, H2Result};

/// MCP サーバーの安全制御・ガードレール設定
#[derive(Debug, Clone)]
pub struct McpSafetyConfig {
    /// デフォルト読み取り専用モード（query_read での更新拒否）
    pub default_read_only: bool,
    /// サーバー全体で書込み操作 (query_write) を許可するか
    pub allow_write: bool,
    /// 1クエリ当たりの最大返却行数ハードリミット
    pub max_rows_hard_limit: usize,
    /// 1クエリ当たりの最大レスポンスバイト数
    pub max_response_bytes: usize,
    /// デフォルトクエリタイムアウト
    pub default_timeout: Duration,
    /// 実行を拒否する危険コマンド
    pub blocked_statements: Vec<String>,
}

impl Default for McpSafetyConfig {
    fn default() -> Self {
        Self {
            default_read_only: true,
            allow_write: false,
            max_rows_hard_limit: 5000,
            max_response_bytes: 4 * 1024 * 1024, // 4 MiB
            default_timeout: Duration::from_secs(5),
            blocked_statements: vec![
                "DROP DATABASE".to_string(),
                "VACUUM".to_string(),
            ],
        }
    }
}

impl McpSafetyConfig {
    pub fn new_permissive() -> Self {
        Self {
            allow_write: true,
            ..Default::default()
        }
    }

    /// SQL 文が危険コマンドとしてブロック対象になっていないか検証
    pub fn check_blocked(&self, sql: &str) -> H2Result<()> {
        let upper = sql.to_ascii_uppercase();
        for blocked in &self.blocked_statements {
            if upper.contains(&blocked.to_ascii_uppercase()) {
                return Err(H2Error::Execution(format!(
                    "Execution blocked by MCP safety policy: command contains forbidden '{blocked}'"
                )));
            }
        }
        Ok(())
    }

    /// 読み取り専用クエリ（SELECT, EXPLAIN, SHOW）であることを検証
    pub fn check_read_only(&self, sql: &str) -> H2Result<()> {
        let trimmed = sql.trim().trim_start_matches(';').trim();
        let first_word = trimmed
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_ascii_uppercase();

        match first_word.as_str() {
            "SELECT" | "EXPLAIN" | "SHOW" | "DESCRIBE" | "DESC" => Ok(()),
            "WITH" => {
                // WITH (CTE) の場合、更新 CTE (INSERT/UPDATE/DELETE) を拒否
                let upper = trimmed.to_ascii_uppercase();
                if upper.contains("INSERT INTO")
                    || upper.contains("UPDATE ")
                    || upper.contains("DELETE FROM")
                {
                    return Err(H2Error::ReadOnly(
                        "Mutating CTEs are not allowed in query_read. Use query_write.".to_string(),
                    ));
                }
                Ok(())
            }
            "INSERT" | "UPDATE" | "DELETE" | "DROP" | "CREATE" | "ALTER" | "TRUNCATE" | "GRANT" | "REVOKE" | "MERGE" => {
                Err(H2Error::ReadOnly(format!(
                    "Operation '{first_word}' is forbidden in query_read. Use query_write with proper authorization."
                )))
            }
            _ => {
                Err(H2Error::ReadOnly(format!(
                    "Unrecognized or non-read-only statement starting with '{first_word}'. Only SELECT, EXPLAIN, and SHOW are allowed in query_read."
                )))
            }
        }
    }
}
