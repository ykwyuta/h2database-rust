use std::sync::Arc;
use h2::Connection;
use h2_types::H2Result;
use serde_json::json;

use crate::formatter::{format_query_results, OutputFormat};
use crate::protocol::{McpResource, McpResourceContent};

/// 公開する MCP Resources 一覧を返却
pub fn list_resources(conn: &Connection) -> Vec<McpResource> {
    let mut resources = vec![
        McpResource {
            uri: "h2://schema".to_string(),
            name: "Database DDL Schema".to_string(),
            description: Some("Complete DDL definition and catalog structures of all tables in the database.".to_string()),
            mime_type: Some("text/x-sql".to_string()),
        },
        McpResource {
            uri: "h2://metrics/live".to_string(),
            name: "Live Query Performance Metrics".to_string(),
            description: Some("Recent query executions, execution times, calls, and performance hot spots.".to_string()),
            mime_type: Some("application/json".to_string()),
        },
    ];

    // 各テーブル用リソース h2://tables/{name}
    if let Ok(rows) = conn.query("SHOW TABLES") {
        for r in rows {
            if let Ok(name) = r.get_as::<String>(0) {
                resources.push(McpResource {
                    uri: format!("h2://tables/{name}"),
                    name: format!("Table: {name}"),
                    description: Some(format!("Schema definition and sample data (top 5 rows) for table '{name}'.")),
                    mime_type: Some("text/markdown".to_string()),
                });
            }
        }
    }

    resources
}

/// 指定 URI のリソース内容を生成して返却
pub fn read_resource(conn: Arc<Connection>, uri: &str) -> H2Result<McpResourceContent> {
    if uri == "h2://schema" {
        // 全テーブルのスキーマ取得
        let tables_res = conn.query("SHOW TABLES")?;
        let mut ddl_out = String::from("-- H2 Database Rust DDL Schema Export\n\n");

        for tr in tables_res {
            let table: String = tr.get_as(0)?;
            ddl_out.push_str(&format!("-- Table: {table}\nCREATE TABLE \"{table}\" (\n"));

            let cols_res = conn.query(&format!("SHOW COLUMNS FROM \"{table}\""))?;
            let mut col_defs = Vec::new();
            for cr in cols_res {
                let col: String = cr.get_as(0)?;
                let dtype: String = cr.get_as(1)?;
                let null: String = cr.get_as(2)?;
                let key: String = cr.get_as(3)?;
                let pri_clause = if key == "PRI" { " PRIMARY KEY" } else { "" };
                let null_clause = if null == "NO" && key != "PRI" { " NOT NULL" } else { "" };
                col_defs.push(format!("    \"{col}\" {dtype}{pri_clause}{null_clause}"));
            }
            ddl_out.push_str(&col_defs.join(",\n"));
            ddl_out.push_str("\n);\n\n");
        }

        return Ok(McpResourceContent {
            uri: uri.to_string(),
            mime_type: Some("text/x-sql".to_string()),
            text: Some(ddl_out),
        });
    }

    if uri.starts_with("h2://tables/") {
        let table_name = &uri["h2://tables/".len()..];
        let mut out = format!("# Table `{table_name}`\n\n## Columns\n");

        // カラム情報
        let cols_res = conn.query(&format!("SHOW COLUMNS FROM \"{table_name}\""))?;
        out.push_str("| Field | Type | Null | Key |\n|---|---|---|---|\n");
        for cr in cols_res {
            let field: String = cr.get_as(0)?;
            let dtype: String = cr.get_as(1)?;
            let null: String = cr.get_as(2)?;
            let key: String = cr.get_as(3)?;
            out.push_str(&format!("| {field} | {dtype} | {null} | {key} |\n"));
        }

        // サンプル 5 行
        out.push_str("\n## Sample Data (Top 5 rows)\n\n");
        let sample_query = format!("SELECT * FROM \"{}\" LIMIT 5", table_name.replace('"', "\"\""));
        if let Ok(h2::ExecutionResult::Query { columns, rows }) = conn.execute_raw(&sample_query) {
            out.push_str(&format_query_results(&columns, &rows, OutputFormat::Markdown, false, rows.len()));
        } else {
            out.push_str("*(No rows available)*\n");
        }

        return Ok(McpResourceContent {
            uri: uri.to_string(),
            mime_type: Some("text/markdown".to_string()),
            text: Some(out),
        });
    }

    if uri == "h2://metrics/live" {
        let stats_rows = conn.query("SHOW QUERY STATS").unwrap_or_default();
        let mut stats_list = Vec::new();

        for r in stats_rows {
            stats_list.push(json!({
                "query": r.get_as::<String>(0).unwrap_or_default(),
                "calls": r.get(1).map(|v| v.to_string()).unwrap_or_default(),
                "totalTime": r.get(2).map(|v| v.to_string()).unwrap_or_default(),
                "rowsScanned": r.get(3).map(|v| v.to_string()).unwrap_or_default(),
            }));
        }

        let body = json!({
            "storageVersion": conn.version(),
            "queryStats": stats_list,
        });

        return Ok(McpResourceContent {
            uri: uri.to_string(),
            mime_type: Some("application/json".to_string()),
            text: Some(serde_json::to_string_pretty(&body).unwrap_or_default()),
        });
    }

    Err(h2_types::H2Error::Execution(format!("Resource not found: '{uri}'")))
}
