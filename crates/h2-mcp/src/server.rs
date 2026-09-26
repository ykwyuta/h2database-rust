use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use axum::extract::{Json, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use futures_util::stream;
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tower_http::cors::CorsLayer;
use tracing::{error, info};

use h2::Connection;
use h2_types::H2Result;

use crate::prompts::{get_prompt, list_prompts};
use crate::protocol::{JsonRpcRequest, JsonRpcResponse};
use crate::resources::{list_resources, read_resource};
use crate::safety::McpSafetyConfig;
use crate::tools::{call_tool, list_tools};

/// MCP Server 状態コンテキスト
#[derive(Clone)]
pub struct McpServerState {
    pub conn: Arc<Connection>,
    pub safety: Arc<McpSafetyConfig>,
    pub server_name: String,
    pub server_version: String,
}

/// Stateless Streamable-HTTP MCP サーバー
pub struct McpServer {
    state: McpServerState,
}

impl McpServer {
    pub fn new(conn: Arc<Connection>, safety: McpSafetyConfig) -> Self {
        Self {
            state: McpServerState {
                conn,
                safety: Arc::new(safety),
                server_name: "h2-mcp".to_string(),
                server_version: env!("CARGO_PKG_VERSION").to_string(),
            },
        }
    }

    /// Axum ルーターを構築
    pub fn router(self) -> Router {
        Router::new()
            .route("/mcp", post(handle_post_mcp).get(handle_get_mcp))
            .route("/health", get(handle_health))
            .layer(CorsLayer::permissive())
            .with_state(self.state)
    }

    /// 指定アドレスにバインドしてバックグラウンドで HTTP サーバーを開始
    pub async fn bind(addr: SocketAddr, conn: Arc<Connection>, safety: McpSafetyConfig) -> H2Result<McpServerHandle> {
        let listener = TcpListener::bind(addr).await?;
        let local_addr = listener.local_addr()?;
        info!("Stateless Streamable-HTTP MCP Server listening on http://{}", local_addr);

        let server = Self::new(conn, safety);
        let router = server.router();

        let handle = tokio::spawn(async move {
            if let Err(e) = axum::serve(listener, router).await {
                error!("MCP server error: {e}");
            }
        });

        Ok(McpServerHandle {
            local_addr,
            join_handle: handle,
        })
    }
}

pub struct McpServerHandle {
    pub local_addr: SocketAddr,
    pub join_handle: tokio::task::JoinHandle<()>,
}

/// GET /health: サーバーヘルスチェック
async fn handle_health(State(state): State<McpServerState>) -> impl IntoResponse {
    let status = json!({
        "status": "ok",
        "server": state.server_name,
        "version": state.server_version,
        "storageVersion": state.conn.version(),
    });
    (StatusCode::OK, Json(status))
}

/// GET /mcp: MCP 2024-11 SSE 初期接続ハンドシェイク
async fn handle_get_mcp() -> Sse<impl futures_util::stream::Stream<Item = Result<Event, Infallible>>> {
    let initial_event = Event::default()
        .event("endpoint")
        .data("/mcp");

    let stream = stream::once(async move { Ok(initial_event) });
    Sse::new(stream).keep_alive(KeepAlive::default())
}

/// POST /mcp: JSON-RPC 2.0 リクエスト処理（JSON レスポンス または SSE ストリーミング）
async fn handle_post_mcp(
    State(state): State<McpServerState>,
    headers: HeaderMap,
    Json(payload): Json<Value>,
) -> Response {
    let is_sse = headers
        .get(header::ACCEPT)
        .and_then(|h| h.to_str().ok())
        .map(|s| s.contains("text/event-stream"))
        .unwrap_or(false);

    // 単一リクエストのパース
    let req: JsonRpcRequest = match serde_json::from_value(payload) {
        Ok(r) => r,
        Err(e) => {
            let resp = JsonRpcResponse::invalid_request(None, format!("Failed to parse JSON-RPC request: {e}"));
            return (StatusCode::BAD_REQUEST, Json(resp)).into_response();
        }
    };

    let id = req.id.clone();
    let method = req.method.clone();
    let params = req.params.clone().unwrap_or(Value::Null);

    // 進捗トークンの検出（tools/call かつ _meta.progressToken が指定されている場合）
    let progress_token = params
        .get("_meta")
        .and_then(|m| m.get("progressToken"))
        .cloned();

    let rpc_response = dispatch_rpc(&state, id.clone(), &method, &params).await;

    if is_sse {
        // SSE ストリーミングモード
        let mut events = Vec::new();

        // 1. 進捗通知イベント（指定時またはストリーミング時）
        if let Some(token) = progress_token {
            let progress_notif = json!({
                "jsonrpc": "2.0",
                "method": "notifications/progress",
                "params": {
                    "progressToken": token,
                    "progress": 100,
                    "total": 100,
                    "message": "Execution complete"
                }
            });
            events.push(Event::default().event("message").data(progress_notif.to_string()));
        }

        // 2. 最終応答イベント
        let final_json = serde_json::to_string(&rpc_response).unwrap_or_default();
        events.push(Event::default().event("message").data(final_json));

        let stream = stream::iter(events.into_iter().map(Ok::<Event, Infallible>));
        Sse::new(stream).keep_alive(KeepAlive::default()).into_response()
    } else {
        // 通常の JSON モード
        (StatusCode::OK, Json(rpc_response)).into_response()
    }
}

/// JSON-RPC 2.0 メソッドディスパッチャ
async fn dispatch_rpc(
    state: &McpServerState,
    id: Option<Value>,
    method: &str,
    params: &Value,
) -> JsonRpcResponse {
    match method {
        "initialize" => {
            let result = json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {
                    "tools": { "listChanged": false },
                    "resources": { "listChanged": false, "subscribe": false },
                    "prompts": { "listChanged": false }
                },
                "serverInfo": {
                    "name": state.server_name,
                    "version": state.server_version
                },
                "instructions": "H2 Database Rust Stateless Streamable-HTTP MCP Server. Use query_read for safe read-only querying, list_tables and describe_table for schema discovery."
            });
            JsonRpcResponse::success(id, result)
        }

        "notifications/initialized" => {
            JsonRpcResponse::success(id, json!({}))
        }

        "ping" => {
            JsonRpcResponse::success(id, json!({}))
        }

        "tools/list" => {
            let tools = list_tools();
            let result = json!({ "tools": tools });
            JsonRpcResponse::success(id, result)
        }

        "tools/call" => {
            let name = match params.get("name").and_then(Value::as_str) {
                Some(n) => n,
                None => return JsonRpcResponse::invalid_params(id, "Missing tool 'name'"),
            };
            let args = params.get("arguments").unwrap_or(&Value::Null);

            match call_tool(Arc::clone(&state.conn), Arc::clone(&state.safety), name, args).await {
                Ok(tool_result) => {
                    let result_val = serde_json::to_value(tool_result).unwrap_or(Value::Null);
                    JsonRpcResponse::success(id, result_val)
                }
                Err(e) => {
                    JsonRpcResponse::internal_error(id, format!("Tool execution error: {e}"))
                }
            }
        }

        "resources/list" => {
            let resources = list_resources(&state.conn);
            let result = json!({ "resources": resources });
            JsonRpcResponse::success(id, result)
        }

        "resources/read" => {
            let uri = match params.get("uri").and_then(Value::as_str) {
                Some(u) => u,
                None => return JsonRpcResponse::invalid_params(id, "Missing resource 'uri'"),
            };

            match read_resource(Arc::clone(&state.conn), uri) {
                Ok(content) => {
                    let result = json!({ "contents": [content] });
                    JsonRpcResponse::success(id, result)
                }
                Err(e) => {
                    JsonRpcResponse::internal_error(id, format!("Resource read error: {e}"))
                }
            }
        }

        "prompts/list" => {
            let prompts = list_prompts();
            let result = json!({ "prompts": prompts });
            JsonRpcResponse::success(id, result)
        }

        "prompts/get" => {
            let name = match params.get("name").and_then(Value::as_str) {
                Some(n) => n,
                None => return JsonRpcResponse::invalid_params(id, "Missing prompt 'name'"),
            };
            let args = params.get("arguments").unwrap_or(&Value::Null);

            match get_prompt(Arc::clone(&state.conn), name, args) {
                Ok(messages) => {
                    let result = json!({
                        "description": format!("Prompt: {name}"),
                        "messages": messages
                    });
                    JsonRpcResponse::success(id, result)
                }
                Err(e) => {
                    JsonRpcResponse::internal_error(id, format!("Prompt error: {e}"))
                }
            }
        }

        other => JsonRpcResponse::method_not_found(id, other),
    }
}
