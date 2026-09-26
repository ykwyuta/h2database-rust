use std::sync::Arc;
use h2::Connection;
use h2_types::H2Result;
use serde_json::Value;

use crate::protocol::{McpContent, McpPrompt, McpPromptArgument, McpPromptMessage};

/// 公開する MCP Prompts 一覧を返却
pub fn list_prompts() -> Vec<McpPrompt> {
    vec![
        McpPrompt {
            name: "sql_analyst".to_string(),
            description: Some("Transform natural language questions into safe, highly optimized H2 SQL queries with schema context.".to_string()),
            arguments: Some(vec![
                McpPromptArgument {
                    name: "question".to_string(),
                    description: Some("The business or analytics question to answer using SQL.".to_string()),
                    required: true,
                }
            ]),
        },
        McpPrompt {
            name: "performance_tuner".to_string(),
            description: Some("Diagnose query performance, inspect EXPLAIN plans, and suggest index or CTE optimizations.".to_string()),
            arguments: Some(vec![
                McpPromptArgument {
                    name: "sql".to_string(),
                    description: Some("The SQL query that needs performance diagnosis.".to_string()),
                    required: true,
                }
            ]),
        },
    ]
}

/// プロンプトのテンプレートを生成
pub fn get_prompt(conn: Arc<Connection>, name: &str, args: &Value) -> H2Result<Vec<McpPromptMessage>> {
    match name {
        "sql_analyst" => {
            let question = args.get("question").and_then(Value::as_str).unwrap_or("");

            // スキーマ情報の取得
            let tables = conn.query("SHOW TABLES").unwrap_or_default();
            let mut schema_brief = String::new();
            for t in tables {
                if let Ok(tbl) = t.get_as::<String>(0) {
                    schema_brief.push_str(&format!("- Table: {tbl}\n"));
                }
            }

            let system_prompt = format!(
                "You are an expert SQL Data Analyst and Architect for H2 Database in Rust.\n\
                 Available Database Tables:\n{schema_brief}\n\
                 Guidelines:\n\
                 1. Prefer read-only queries with `query_read` tool.\n\
                 2. Use markdown formatting for output.\n\
                 3. H2 Database supports standard SQL, window functions, CTEs, and json operators (->, ->>).\n\n\
                 User Question: {question}"
            );

            Ok(vec![
                McpPromptMessage {
                    role: "user".to_string(),
                    content: McpContent::text(system_prompt),
                }
            ])
        }
        "performance_tuner" => {
            let sql = args.get("sql").and_then(Value::as_str).unwrap_or("");
            let explain_res = conn.query(&format!("EXPLAIN {sql}")).ok();
            let mut plan = String::new();
            if let Some(rows) = explain_res {
                for r in rows {
                    if let Some(v) = r.get(0) {
                        plan.push_str(&v.to_string());
                        plan.push('\n');
                    }
                }
            }

            let prompt = format!(
                "You are a Database Performance Tuning Specialist.\n\
                 Target SQL:\n```sql\n{sql}\n```\n\n\
                 EXPLAIN Plan:\n```text\n{plan}```\n\n\
                 Please analyze the execution plan and provide:\n\
                 1. Potential bottlenecks (e.g. SeqScan vs IndexScan)\n\
                 2. Recommended CREATE INDEX statements\n\
                 3. Query rewrite opportunities."
            );

            Ok(vec![
                McpPromptMessage {
                    role: "user".to_string(),
                    content: McpContent::text(prompt),
                }
            ])
        }
        other => Err(h2_types::H2Error::Execution(format!("Unknown prompt: '{other}'"))),
    }
}
