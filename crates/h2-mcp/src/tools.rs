use std::sync::Arc;
use std::time::Duration;
use h2::{Connection, ExecutionResult};
use h2_types::H2Result;
use serde_json::{json, Value};

use crate::formatter::{format_query_results, OutputFormat};
use crate::protocol::{McpTool, McpToolResult};
use crate::safety::McpSafetyConfig;

/// 公開する全 MCP ツールの一覧を返却
pub fn list_tools() -> Vec<McpTool> {
    vec![
        McpTool {
            name: "query_read".to_string(),
            description: "Execute a safe read-only SQL query (SELECT, EXPLAIN, SHOW) with token-efficient formatting and truncation limits.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "sql": {
                        "type": "string",
                        "description": "The read-only SQL query to execute (e.g. SELECT, EXPLAIN, SHOW)"
                    },
                    "format": {
                        "type": "string",
                        "enum": ["markdown", "compact_json", "json", "csv"],
                        "default": "markdown",
                        "description": "Output format. Markdown is most token-efficient for LLMs; compact_json is columnar; json is standard."
                    },
                    "max_rows": {
                        "type": "integer",
                        "default": 100,
                        "maximum": 5000,
                        "description": "Maximum number of rows to return before truncating."
                    },
                    "timeout_ms": {
                        "type": "integer",
                        "default": 5000,
                        "description": "Query execution timeout in milliseconds."
                    }
                },
                "required": ["sql"]
            }),
        },
        McpTool {
            name: "query_write".to_string(),
            description: "Execute a data modification SQL statement (INSERT, UPDATE, DELETE, CREATE, DROP). Supports dry_run rollback validation.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "sql": {
                        "type": "string",
                        "description": "The SQL DDL/DML statement to execute."
                    },
                    "dry_run": {
                        "type": "boolean",
                        "default": false,
                        "description": "If true, executes within a transaction and rolls back, returning the estimated affected row count."
                    }
                },
                "required": ["sql"]
            }),
        },
        McpTool {
            name: "list_tables".to_string(),
            description: "List all user tables, views, and queue tables in the database with their schemas, types, and row counts.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {}
            }),
        },
        McpTool {
            name: "describe_table".to_string(),
            description: "Get detailed column definitions, data types, nullability, defaults, and primary key status for a specific table.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "table_name": {
                        "type": "string",
                        "description": "Target table name to inspect."
                    }
                },
                "required": ["table_name"]
            }),
        },
        McpTool {
            name: "explain_query".to_string(),
            description: "Explain the SQL execution plan and analyze index usage for query optimization.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "sql": {
                        "type": "string",
                        "description": "The SQL query to diagnose."
                    }
                },
                "required": ["sql"]
            }),
        },
        McpTool {
            name: "get_table_statistics".to_string(),
            description: "Retrieve performance metrics, statement execution frequencies, and storage statistics.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "table_name": {
                        "type": "string",
                        "description": "Optional table name for specific row metrics."
                    }
                }
            }),
        },
    ]
}

/// ツールのディスパッチと実行
pub async fn call_tool(
    conn: Arc<Connection>,
    safety: Arc<McpSafetyConfig>,
    name: &str,
    args: &Value,
) -> H2Result<McpToolResult> {
    match name {
        "query_read" => execute_query_read(&conn, &safety, args),
        "query_write" => execute_query_write(&conn, &safety, args),
        "list_tables" => execute_list_tables(&conn),
        "describe_table" => execute_describe_table(&conn, args),
        "explain_query" => execute_explain_query(&conn, args),
        "get_table_statistics" => execute_get_table_statistics(&conn, args),
        other => Ok(McpToolResult::error(format!("Unknown tool: '{other}'"))),
    }
}

fn execute_query_read(
    conn: &Connection,
    safety: &McpSafetyConfig,
    args: &Value,
) -> H2Result<McpToolResult> {
    let sql = match args.get("sql").and_then(Value::as_str) {
        Some(s) if !s.trim().is_empty() => s,
        _ => return Ok(McpToolResult::error("Missing required argument 'sql'")),
    };

    // 1. ガードレール検証
    if let Err(e) = safety.check_blocked(sql) {
        return Ok(McpToolResult::error(e.to_string()));
    }
    if safety.default_read_only {
        if let Err(e) = safety.check_read_only(sql) {
            return Ok(McpToolResult::error(e.to_string()));
        }
    }

    let format_str = args.get("format").and_then(Value::as_str).unwrap_or("markdown");
    let format = OutputFormat::parse(format_str);

    let max_rows = args
        .get("max_rows")
        .and_then(Value::as_u64)
        .map(|n| (n as usize).min(safety.max_rows_hard_limit))
        .unwrap_or(100);

    let timeout_ms = args
        .get("timeout_ms")
        .and_then(Value::as_u64)
        .map(Duration::from_millis)
        .unwrap_or(safety.default_timeout);

    let _guard = h2_types::set_query_timeout(Some(timeout_ms));

    // 2. 実行
    match conn.execute_raw(sql) {
        Ok(ExecutionResult::Query { columns, rows }) => {
            let total_fetched = rows.len();
            let is_truncated = total_fetched > max_rows;
            let displayed_rows = if is_truncated { &rows[..max_rows] } else { &rows[..] };

            let formatted = format_query_results(&columns, displayed_rows, format, is_truncated, total_fetched);
            Ok(McpToolResult::text(formatted))
        }
        Ok(ExecutionResult::Dml { affected_rows }) => {
            Ok(McpToolResult::text(format!("Query executed. Affected rows: {affected_rows}")))
        }
        Ok(ExecutionResult::Ddl) => {
            Ok(McpToolResult::text("DDL executed successfully."))
        }
        Err(e) => {
            Ok(McpToolResult::error(format!("Query failed: {e}")))
        }
    }
}

fn execute_query_write(
    conn: &Connection,
    safety: &McpSafetyConfig,
    args: &Value,
) -> H2Result<McpToolResult> {
    let sql = match args.get("sql").and_then(Value::as_str) {
        Some(s) if !s.trim().is_empty() => s,
        _ => return Ok(McpToolResult::error("Missing required argument 'sql'")),
    };

    if let Err(e) = safety.check_blocked(sql) {
        return Ok(McpToolResult::error(e.to_string()));
    }

    let dry_run = args.get("dry_run").and_then(Value::as_bool).unwrap_or(false);

    if dry_run {
        // トランザクション内で実行し、即座に明示的ロールバック
        let tx = match conn.transaction() {
            Ok(tx) => tx,
            Err(e) => return Ok(McpToolResult::error(format!("Failed to begin dry-run transaction: {e}"))),
        };

        match tx.execute(sql) {
            Ok(affected) => {
                let _ = tx.rollback();
                Ok(McpToolResult::text(format!(
                    "🔍 [DRY RUN SUCCESS] Statement parsed and executed successfully.\n- Planned affected rows: {affected}\n- Changes were safely ROLLED BACK."
                )))
            }
            Err(e) => {
                let _ = tx.rollback();
                Ok(McpToolResult::error(format!("Dry-run execution failed: {e}")))
            }
        }
    } else {
        if !safety.allow_write {
            return Ok(McpToolResult::error(
                "Write operations (query_write) are currently disabled on this MCP server. Enable with server setting 'allow_write: true' or use dry_run: true."
            ));
        }

        match conn.execute_raw(sql) {
            Ok(ExecutionResult::Dml { affected_rows }) => {
                Ok(McpToolResult::text(format!("✅ Modification committed. Affected rows: {affected_rows}")))
            }
            Ok(ExecutionResult::Ddl) => {
                Ok(McpToolResult::text("✅ DDL statement executed and schema committed successfully."))
            }
            Ok(ExecutionResult::Query { columns, rows }) => {
                let formatted = format_query_results(&columns, &rows, OutputFormat::Markdown, false, rows.len());
                Ok(McpToolResult::text(formatted))
            }
            Err(e) => Ok(McpToolResult::error(format!("Execution failed: {e}"))),
        }
    }
}

fn execute_list_tables(conn: &Connection) -> H2Result<McpToolResult> {
    match conn.query("SHOW TABLES") {
        Ok(rows) => {
            let mut out = String::new();
            out.push_str("| Table Name | Est. Rows |\n|---|---|\n");

            for r in rows {
                let table: String = r.get_as(0).unwrap_or_default();
                let count_query = format!("SELECT count(*) FROM \"{}\"", table.replace('"', "\"\""));
                let count_str = conn.query(&count_query)
                    .ok()
                    .and_then(|cr| cr.first().and_then(|crow| crow.get(0).map(|v| v.to_string())))
                    .unwrap_or_else(|| "N/A".to_string());

                out.push_str(&format!("| {table} | {count_str} |\n"));
            }
            Ok(McpToolResult::text(out))
        }
        Err(e) => Ok(McpToolResult::error(format!("Failed to list tables: {e}"))),
    }
}

fn execute_describe_table(conn: &Connection, args: &Value) -> H2Result<McpToolResult> {
    let table_name = match args.get("table_name").and_then(Value::as_str) {
        Some(t) => t,
        None => return Ok(McpToolResult::error("Missing required argument 'table_name'")),
    };

    let sql = format!("SHOW COLUMNS FROM \"{}\"", table_name.replace('"', "\"\""));
    match conn.query(&sql) {
        Ok(rows) if !rows.is_empty() => {
            let mut out = format!("### Schema definition for table `{table_name}`\n\n");
            out.push_str("| Field | Type | Null | Key |\n|---|---|---|---|\n");
            for r in rows {
                let field: String = r.get_as(0).unwrap_or_default();
                let dtype: String = r.get_as(1).unwrap_or_default();
                let null: String = r.get_as(2).unwrap_or_default();
                let key: String = r.get_as(3).unwrap_or_default();
                out.push_str(&format!("| {field} | {dtype} | {null} | {key} |\n"));
            }
            Ok(McpToolResult::text(out))
        }
        Ok(_) => Ok(McpToolResult::error(format!("Table '{table_name}' was not found in schema."))),
        Err(e) => Ok(McpToolResult::error(format!("Failed to describe table: {e}"))),
    }
}

fn execute_explain_query(conn: &Connection, args: &Value) -> H2Result<McpToolResult> {
    let sql = match args.get("sql").and_then(Value::as_str) {
        Some(s) => s,
        None => return Ok(McpToolResult::error("Missing required argument 'sql'")),
    };

    let explain_sql = format!("EXPLAIN {sql}");
    match conn.query(&explain_sql) {
        Ok(rows) => {
            let mut plan = String::new();
            for r in rows {
                if let Some(v) = r.get(0) {
                    plan.push_str(&v.to_string());
                    plan.push('\n');
                }
            }
            Ok(McpToolResult::text(format!("```text\n{plan}```")))
        }
        Err(e) => Ok(McpToolResult::error(format!("Failed to explain query: {e}"))),
    }
}

fn execute_get_table_statistics(conn: &Connection, args: &Value) -> H2Result<McpToolResult> {
    let table_filter = args.get("table_name").and_then(Value::as_str);

    match conn.query("SHOW QUERY STATS") {
        Ok(rows) => {
            let mut out = String::from("### Query and Execution Statistics\n\n");
            out.push_str("| Query Fingerprint | Calls | Total Time | Rows Scanned |\n|---|---|---|---|\n");
            let mut matched = 0;
            for r in rows {
                let query: String = r.get_as(0).unwrap_or_default();
                if let Some(filter) = table_filter {
                    if !query.to_ascii_lowercase().contains(&filter.to_ascii_lowercase()) {
                        continue;
                    }
                }
                let calls: String = r.get(1).map(|v| v.to_string()).unwrap_or_default();
                let time: String = r.get(2).map(|v| v.to_string()).unwrap_or_default();
                let rows_cnt: String = r.get(3).map(|v| v.to_string()).unwrap_or_default();
                out.push_str(&format!("| `{query}` | {calls} | {time} | {rows_cnt} |\n"));
                matched += 1;
            }
            if matched == 0 {
                out.push_str("| *(No query stats recorded yet)* | - | - | - |\n");
            }
            Ok(McpToolResult::text(out))
        }
        Err(e) => Ok(McpToolResult::error(format!("Failed to retrieve statistics: {e}"))),
    }
}
