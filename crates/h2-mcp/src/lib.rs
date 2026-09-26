pub mod protocol;
pub mod formatter;
pub mod safety;
pub mod tools;
pub mod resources;
pub mod prompts;
pub mod server;

pub use formatter::{format_query_results, OutputFormat};
pub use protocol::{
    JsonRpcError, JsonRpcRequest, JsonRpcResponse, McpContent, McpPrompt, McpResource,
    McpResourceContent, McpTool, McpToolResult,
};
pub use safety::McpSafetyConfig;
pub use server::{McpServer, McpServerHandle, McpServerState};

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use axum::http::{header, StatusCode};
    use h2::Connection;
    use serde_json::json;
    use tower::ServiceExt;

    fn setup_test_db() -> Arc<Connection> {
        let conn = Arc::new(Connection::open_in_memory().unwrap());
        conn.execute("CREATE TABLE users (id INT PRIMARY KEY, name VARCHAR, balance DECIMAL(10,2))").unwrap();
        conn.execute("INSERT INTO users VALUES (1, 'Alice', 100.50), (2, 'Bob', 250.00), (3, 'Charlie', 50.00)").unwrap();
        conn
    }

    #[tokio::test]
    async fn test_mcp_initialize_and_ping() {
        let conn = setup_test_db();
        let server = McpServer::new(conn, McpSafetyConfig::default());
        let router = server.router();

        // 1. initialize
        let init_req = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "clientInfo": { "name": "test-client", "version": "1.0" }
            }
        });

        let resp = router
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/mcp")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(axum::body::Body::from(serde_json::to_vec(&init_req).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 64).await.unwrap();
        let json_resp: JsonRpcResponse = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json_resp.id, Some(json!(1)));
        assert!(json_resp.result.is_some());
        let res = json_resp.result.unwrap();
        assert_eq!(res["protocolVersion"], "2024-11-05");
        assert_eq!(res["serverInfo"]["name"], "h2-mcp");

        // 2. ping
        let ping_req = json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "ping"
        });

        let resp2 = router
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/mcp")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(axum::body::Body::from(serde_json::to_vec(&ping_req).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp2.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_mcp_query_read_formats_and_safety() {
        let conn = setup_test_db();
        let server = McpServer::new(conn, McpSafetyConfig::default());
        let router = server.router();

        // 1. query_read in Markdown
        let req_md = json!({
            "jsonrpc": "2.0",
            "id": 10,
            "method": "tools/call",
            "params": {
                "name": "query_read",
                "arguments": {
                    "sql": "SELECT id, name, balance FROM users ORDER BY id",
                    "format": "markdown"
                }
            }
        });

        let resp = router
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/mcp")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(axum::body::Body::from(serde_json::to_vec(&req_md).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 64).await.unwrap();
        let json_resp: JsonRpcResponse = serde_json::from_slice(&bytes).unwrap();
        let result = json_resp.result.unwrap();
        assert_eq!(result["isError"], false);
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("| id | name | balance |"));
        assert!(text.contains("| 1 | Alice | 100.50 |"));

        // 2. query_read in Compact JSON
        let req_compact = json!({
            "jsonrpc": "2.0",
            "id": 11,
            "method": "tools/call",
            "params": {
                "name": "query_read",
                "arguments": {
                    "sql": "SELECT id, name FROM users WHERE id = 1",
                    "format": "compact_json"
                }
            }
        });

        let resp2 = router
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/mcp")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(axum::body::Body::from(serde_json::to_vec(&req_compact).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        let bytes2 = axum::body::to_bytes(resp2.into_body(), 1024 * 64).await.unwrap();
        let json_resp2: JsonRpcResponse = serde_json::from_slice(&bytes2).unwrap();
        let text2 = json_resp2.result.unwrap()["content"][0]["text"].as_str().unwrap().to_string();
        assert!(text2.contains("\"columns\":[\"id\",\"name\"]"));
        assert!(text2.contains("Alice"));

        // 3. Safety rejection: attempting UPDATE or DROP in query_read
        let req_unsafe = json!({
            "jsonrpc": "2.0",
            "id": 12,
            "method": "tools/call",
            "params": {
                "name": "query_read",
                "arguments": {
                    "sql": "DELETE FROM users WHERE id = 1"
                }
            }
        });

        let resp3 = router
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/mcp")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(axum::body::Body::from(serde_json::to_vec(&req_unsafe).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        let bytes3 = axum::body::to_bytes(resp3.into_body(), 1024 * 64).await.unwrap();
        let json_resp3: JsonRpcResponse = serde_json::from_slice(&bytes3).unwrap();
        let res3 = json_resp3.result.unwrap();
        assert_eq!(res3["isError"], true);
        assert!(res3["content"][0]["text"].as_str().unwrap().contains("forbidden in query_read"));
    }

    #[tokio::test]
    async fn test_mcp_query_write_dry_run_and_execution() {
        let conn = setup_test_db();
        let safety = McpSafetyConfig {
            allow_write: true,
            ..Default::default()
        };
        let server = McpServer::new(Arc::clone(&conn), safety);
        let router = server.router();

        // 1. Dry run modification (should roll back)
        let req_dry = json!({
            "jsonrpc": "2.0",
            "id": 20,
            "method": "tools/call",
            "params": {
                "name": "query_write",
                "arguments": {
                    "sql": "INSERT INTO users VALUES (99, 'Ghost', 999.00)",
                    "dry_run": true
                }
            }
        });

        let resp = router
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/mcp")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(axum::body::Body::from(serde_json::to_vec(&req_dry).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 64).await.unwrap();
        let json_resp: JsonRpcResponse = serde_json::from_slice(&bytes).unwrap();
        let text = json_resp.result.unwrap()["content"][0]["text"].as_str().unwrap().to_string();
        assert!(text.contains("DRY RUN SUCCESS"));
        assert!(text.contains("Planned affected rows: 1"));

        // Dry run did not commit
        let rows = conn.query("SELECT * FROM users WHERE id = 99").unwrap();
        assert_eq!(rows.len(), 0);

        // 2. Real modification execution
        let req_real = json!({
            "jsonrpc": "2.0",
            "id": 21,
            "method": "tools/call",
            "params": {
                "name": "query_write",
                "arguments": {
                    "sql": "INSERT INTO users VALUES (10, 'RealUser', 300.00)",
                    "dry_run": false
                }
            }
        });

        let resp2 = router
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/mcp")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(axum::body::Body::from(serde_json::to_vec(&req_real).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        let bytes2 = axum::body::to_bytes(resp2.into_body(), 1024 * 64).await.unwrap();
        let json_resp2: JsonRpcResponse = serde_json::from_slice(&bytes2).unwrap();
        assert_eq!(json_resp2.result.unwrap()["isError"], false);

        // Committed row exists
        let rows2 = conn.query("SELECT * FROM users WHERE id = 10").unwrap();
        assert_eq!(rows2.len(), 1);
    }

    #[tokio::test]
    async fn test_mcp_metadata_resources_and_prompts() {
        let conn = setup_test_db();
        let server = McpServer::new(conn, McpSafetyConfig::default());
        let router = server.router();

        // 1. list_tables tool
        let req_tables = json!({
            "jsonrpc": "2.0",
            "id": 30,
            "method": "tools/call",
            "params": { "name": "list_tables" }
        });
        let resp = router
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/mcp")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(axum::body::Body::from(serde_json::to_vec(&req_tables).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 64).await.unwrap();
        let json_resp: JsonRpcResponse = serde_json::from_slice(&bytes).unwrap();
        assert!(json_resp.result.unwrap()["content"][0]["text"].as_str().unwrap().contains("users"));

        // 2. describe_table tool
        let req_desc = json!({
            "jsonrpc": "2.0",
            "id": 31,
            "method": "tools/call",
            "params": {
                "name": "describe_table",
                "arguments": { "table_name": "users" }
            }
        });
        let resp2 = router
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/mcp")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(axum::body::Body::from(serde_json::to_vec(&req_desc).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let bytes2 = axum::body::to_bytes(resp2.into_body(), 1024 * 64).await.unwrap();
        let json_resp2: JsonRpcResponse = serde_json::from_slice(&bytes2).unwrap();
        let desc_text = json_resp2.result.unwrap()["content"][0]["text"].as_str().unwrap().to_string();
        assert!(desc_text.contains("balance"));

        // 3. resources/read h2://schema
        let req_res = json!({
            "jsonrpc": "2.0",
            "id": 32,
            "method": "resources/read",
            "params": { "uri": "h2://schema" }
        });
        let resp3 = router
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/mcp")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(axum::body::Body::from(serde_json::to_vec(&req_res).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let bytes3 = axum::body::to_bytes(resp3.into_body(), 1024 * 64).await.unwrap();
        let json_resp3: JsonRpcResponse = serde_json::from_slice(&bytes3).unwrap();
        let ddl_text = json_resp3.result.unwrap()["contents"][0]["text"].as_str().unwrap().to_string();
        assert!(ddl_text.contains("CREATE TABLE \"users\""));

        // 4. prompts/get sql_analyst
        let req_prompt = json!({
            "jsonrpc": "2.0",
            "id": 33,
            "method": "prompts/get",
            "params": {
                "name": "sql_analyst",
                "arguments": { "question": "What is the total balance of all users?" }
            }
        });
        let resp4 = router
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/mcp")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(axum::body::Body::from(serde_json::to_vec(&req_prompt).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let bytes4 = axum::body::to_bytes(resp4.into_body(), 1024 * 64).await.unwrap();
        let json_resp4: JsonRpcResponse = serde_json::from_slice(&bytes4).unwrap();
        let prompt_text = json_resp4.result.unwrap()["messages"][0]["content"]["text"].as_str().unwrap().to_string();
        assert!(prompt_text.contains("User Question: What is the total balance of all users?"));
    }

    #[tokio::test]
    async fn test_mcp_sse_streaming_over_tcp() {
        let conn = setup_test_db();
        let handle = McpServer::bind("127.0.0.1:0".parse().unwrap(), conn, McpSafetyConfig::default())
            .await
            .unwrap();

        let client = reqwest::Client::new();
        let base_url = format!("http://{}", handle.local_addr);

        // 1. Health check
        let health_res = client.get(format!("{base_url}/health")).send().await.unwrap();
        assert_eq!(health_res.status(), 200);
        let health_json: serde_json::Value = health_res.json().await.unwrap();
        assert_eq!(health_json["status"], "ok");

        // 2. POST /mcp with Accept: text/event-stream
        let stream_req = json!({
            "jsonrpc": "2.0",
            "id": 50,
            "method": "tools/call",
            "params": {
                "name": "query_read",
                "arguments": {
                    "sql": "SELECT id, name FROM users"
                },
                "_meta": {
                    "progressToken": 123
                }
            }
        });

        let sse_res = client
            .post(format!("{base_url}/mcp"))
            .header("Accept", "text/event-stream")
            .json(&stream_req)
            .send()
            .await
            .unwrap();

        assert_eq!(sse_res.status(), 200);
        let content_type = sse_res.headers().get("content-type").unwrap().to_str().unwrap();
        assert!(content_type.contains("text/event-stream"));

        let body = sse_res.text().await.unwrap();
        assert!(body.contains("notifications/progress"));
        assert!(body.contains("Alice"));
        assert!(body.contains("progressToken"));

        // Clean up server
        handle.join_handle.abort();
    }
}
