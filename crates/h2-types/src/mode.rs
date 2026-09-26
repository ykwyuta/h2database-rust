use serde::{Deserialize, Serialize};
use std::fmt;

/// SQL 方言互換モード (SQL Dialect Compatibility Mode)
/// 
/// 本家 H2 Database の `SET MODE <name>` に準拠し、
/// 主要商用・オープンソース RDBMS との構文・データ型・関数・セマンティクス互換性を提供します。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum SqlDialectMode {
    /// 標準独自モード (H2 デフォルト)
    #[default]
    Regular,
    /// PostgreSQL 互換モード
    PostgreSql,
    /// MySQL / MariaDB 互換モード
    MySql,
    /// Oracle 互換モード
    Oracle,
    /// Microsoft SQL Server (T-SQL) 互換モード
    MsSqlServer,
    /// IBM DB2 互換モード
    Db2,
}

impl SqlDialectMode {
    /// モードの標準文字列表現（大文字）
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Regular => "REGULAR",
            Self::PostgreSql => "POSTGRESQL",
            Self::MySql => "MYSQL",
            Self::Oracle => "ORACLE",
            Self::MsSqlServer => "MSSQLSERVER",
            Self::Db2 => "DB2",
        }
    }

    /// 文字列から方言モードを解決（大文字小文字無視、各種エイリアス対応）
    pub fn parse_str(s: &str) -> Option<Self> {
        let trimmed = s.trim().trim_matches('\'').trim_matches('"').trim();
        let upper = trimmed.to_uppercase();
        match upper.as_str() {
            "REGULAR" | "H2" | "DEFAULT" => Some(Self::Regular),
            "POSTGRESQL" | "POSTGRES" | "PG" => Some(Self::PostgreSql),
            "MYSQL" | "MARIADB" => Some(Self::MySql),
            "ORACLE" => Some(Self::Oracle),
            "MSSQLSERVER" | "MSSQL" | "SQLSERVER" | "T-SQL" | "TSQL" => Some(Self::MsSqlServer),
            "DB2" => Some(Self::Db2),
            _ => None,
        }
    }

    /// Oracle 互換: 空文字列 `''` を `NULL` として扱うか
    pub fn empty_strings_are_null(&self) -> bool {
        matches!(self, Self::Oracle)
    }

    /// SQL Server 互換: `+` 演算子を文字列連結として扱うか
    pub fn plus_as_string_concat(&self) -> bool {
        matches!(self, Self::MsSqlServer)
    }

    /// `FROM DUAL` 仮想テーブルをサポートするか
    pub fn support_dual_table(&self) -> bool {
        matches!(self, Self::Oracle | Self::MySql | Self::Regular | Self::Db2)
    }
}

impl fmt::Display for SqlDialectMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_mode_aliases() {
        assert_eq!(SqlDialectMode::parse_str("MySQL"), Some(SqlDialectMode::MySql));
        assert_eq!(SqlDialectMode::parse_str("mariadb"), Some(SqlDialectMode::MySql));
        assert_eq!(SqlDialectMode::parse_str("PostgreSQL"), Some(SqlDialectMode::PostgreSql));
        assert_eq!(SqlDialectMode::parse_str("postgres"), Some(SqlDialectMode::PostgreSql));
        assert_eq!(SqlDialectMode::parse_str("pg"), Some(SqlDialectMode::PostgreSql));
        assert_eq!(SqlDialectMode::parse_str("Oracle"), Some(SqlDialectMode::Oracle));
        assert_eq!(SqlDialectMode::parse_str("MSSQLServer"), Some(SqlDialectMode::MsSqlServer));
        assert_eq!(SqlDialectMode::parse_str("sqlserver"), Some(SqlDialectMode::MsSqlServer));
        assert_eq!(SqlDialectMode::parse_str("T-SQL"), Some(SqlDialectMode::MsSqlServer));
        assert_eq!(SqlDialectMode::parse_str("DB2"), Some(SqlDialectMode::Db2));
        assert_eq!(SqlDialectMode::parse_str("REGULAR"), Some(SqlDialectMode::Regular));
        assert_eq!(SqlDialectMode::parse_str("h2"), Some(SqlDialectMode::Regular));
        assert_eq!(SqlDialectMode::parse_str("derby"), None);
    }
}
