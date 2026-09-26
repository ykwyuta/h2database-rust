//! SQL Server T-SQL (Transact-SQL) 手続き言語拡張モジュール
//! 
//! 将来的な SQL Server T-SQL 互換性拡張のためのフロントエンド構文解析・マッピング基盤です。
//! T-SQL の構文要素（@変数, SET @var = expr, IF cond BEGIN ... END, WHILE cond BEGIN ... END,
//! BREAK / CONTINUE, THROW / RAISERROR, PRINT）を共通の Canonical Procedural AST (`ProcAst`) へ
//! 正規化して返却します。

use h2_types::{H2Error, H2Result};
use crate::procedural::ast::{ProcBlock, RoutineDef};

/// T-SQL プロシージャ／バッチ DDL の解析（スタブ／将来拡張用インターフェース）
pub fn parse_tsql_routine(_ddl: &str) -> H2Result<RoutineDef> {
    Err(H2Error::Execution("SQL Server T-SQL procedure syntax parser will be activated in next phase".to_string()))
}

/// T-SQL バッチ本文の解析（スタブ／将来拡張用インターフェース）
pub fn parse_tsql_block(_batch: &str) -> H2Result<ProcBlock> {
    Err(H2Error::Execution("SQL Server T-SQL batch parser will be activated in next phase".to_string()))
}
