use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tracing::{error, info};

use h2_sql::{ExecutionResult, SQLEngine};
use h2_types::{H2Error, H2Result};
use crate::protocol::{PgMessageBuilder, SSL_REQUEST_CODE};

pub struct PgServer {
    listener: TcpListener,
    engine: Arc<SQLEngine>,
}

impl PgServer {
    pub async fn bind(addr: SocketAddr, engine: Arc<SQLEngine>) -> H2Result<Self> {
        let listener = TcpListener::bind(addr).await?;
        info!("PostgreSQL wire server listening on {}", addr);
        Ok(Self { listener, engine })
    }

    pub fn local_addr(&self) -> H2Result<SocketAddr> {
        Ok(self.listener.local_addr()?)
    }

    /// サーバー実行ループ（非同期タスクとして起動可能）
    pub async fn run(self) -> H2Result<()> {
        loop {
            let (socket, client_addr) = self.listener.accept().await?;
            let engine = Arc::clone(&self.engine);

            tokio::spawn(async move {
                if let Err(e) = handle_client(socket, client_addr, engine).await {
                    error!("Error handling client {}: {:?}", client_addr, e);
                }
            });
        }
    }
}

async fn handle_client(mut stream: TcpStream, client_addr: SocketAddr, engine: Arc<SQLEngine>) -> H2Result<()> {
    // 1. ハンドシェイク (SSLRequest & StartupMessage)
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).await?;
    let mut msg_len = u32::from_be_bytes(len_buf) as usize;

    let mut code_buf = [0u8; 4];
    stream.read_exact(&mut code_buf).await?;
    let code = u32::from_be_bytes(code_buf);

    if code == SSL_REQUEST_CODE {
        // SSL 非対応 ('N') を返して平文接続を要求
        stream.write_all(b"N").await?;
        stream.flush().await?;

        // 再度 StartupMessage を読み取り
        stream.read_exact(&mut len_buf).await?;
        msg_len = u32::from_be_bytes(len_buf) as usize;

        stream.read_exact(&mut code_buf).await?;
    }

    // 残りの StartupMessage パラメータを読み取り
    let mut startup_params = std::collections::HashMap::new();
    if msg_len > 8 {
        let mut rest_buf = vec![0u8; msg_len - 8];
        stream.read_exact(&mut rest_buf).await?;
        let mut offset = 0;
        while offset < rest_buf.len() {
            let mut end = offset;
            while end < rest_buf.len() && rest_buf[end] != 0 {
                end += 1;
            }
            if end >= rest_buf.len() {
                break;
            }
            let key = String::from_utf8_lossy(&rest_buf[offset..end]).to_string();
            offset = end + 1;
            if key.is_empty() {
                break;
            }
            let mut val_end = offset;
            while val_end < rest_buf.len() && rest_buf[val_end] != 0 {
                val_end += 1;
            }
            if val_end >= rest_buf.len() {
                break;
            }
            let val = String::from_utf8_lossy(&rest_buf[offset..val_end]).to_string();
            offset = val_end + 1;
            startup_params.insert(key, val);
        }
    }

    let username = startup_params.get("user").cloned().unwrap_or_else(|| "postgres".to_string());
    let client_ip = client_addr.ip().to_string();

    // ユーザー情報確認 & 認証フロー
    let user_info = engine.auth().list_users().into_iter().find(|u| u.username.eq_ignore_ascii_case(&username));
    let has_password = user_info.as_ref().and_then(|u| u.password.as_ref()).is_some();

    let authenticated_password = if has_password {
        stream.write_all(&PgMessageBuilder::authentication_cleartext_password()).await?;
        stream.flush().await?;

        let mut p_type = [0u8; 1];
        stream.read_exact(&mut p_type).await?;
        if p_type[0] != b'p' {
            stream.write_all(&PgMessageBuilder::error_response("Password authentication expected")).await?;
            stream.flush().await?;
            return Err(H2Error::Authentication("Password expected".to_string()));
        }
        let mut p_len_buf = [0u8; 4];
        stream.read_exact(&mut p_len_buf).await?;
        let p_len = (u32::from_be_bytes(p_len_buf) as usize).saturating_sub(4);
        let mut p_buf = vec![0u8; p_len];
        stream.read_exact(&mut p_buf).await?;
        let pwd = String::from_utf8_lossy(&p_buf).trim_matches('\0').to_string();
        Some(pwd)
    } else {
        None
    };

    if let Err(e) = engine.auth().authenticate(&username, authenticated_password.as_deref(), &client_ip) {
        stream.write_all(&PgMessageBuilder::error_response(&e.to_string())).await?;
        stream.flush().await?;
        return Err(e);
    }

    // 認証成功と初期パラメータ、ReadyForQuery を送信
    stream.write_all(&PgMessageBuilder::authentication_ok()).await?;
    stream.write_all(&PgMessageBuilder::parameter_status("server_version", "15.0 (h2-rust)")).await?;
    stream.write_all(&PgMessageBuilder::parameter_status("client_encoding", "UTF8")).await?;
    stream.write_all(&PgMessageBuilder::ready_for_query(b'I')).await?;
    stream.flush().await?;

    // 2. コマンド処理ループ
    // 2. コマンド処理ループ
    let mut active_tx: Option<h2_mvstore::Transaction> = None;
    let mut type_buf = [0u8; 1];
    loop {
        let read_bytes = stream.read(&mut type_buf).await?;
        if read_bytes == 0 {
            if let Some(tx) = active_tx.take() {
                let _ = tokio::task::spawn_blocking(move || tx.rollback()).await;
            }
            break; // 接続終了
        }

        let msg_type = type_buf[0];
        stream.read_exact(&mut len_buf).await?;
        let payload_len = (u32::from_be_bytes(len_buf) as usize).saturating_sub(4);

        let mut payload = vec![0u8; payload_len];
        stream.read_exact(&mut payload).await?;

        match msg_type {
            b'Q' => {
                // Simple Query
                let sql = String::from_utf8_lossy(&payload)
                    .trim_matches('\0')
                    .trim()
                    .to_string();

                let trimmed = sql.trim_end_matches(';').trim();
                let upper = trimmed.to_uppercase();

                if trimmed.is_empty() {
                    let tx_status = if active_tx.is_some() { b'T' } else { b'I' };
                    stream.write_all(&PgMessageBuilder::command_complete("")).await?;
                    stream.write_all(&PgMessageBuilder::ready_for_query(tx_status)).await?;
                    stream.flush().await?;
                    continue;
                }

                let mut out_buf = Vec::with_capacity(512);

                // トランザクション制御コマンド
                if upper == "BEGIN" || upper.starts_with("BEGIN ") || upper == "START TRANSACTION" {
                    if active_tx.is_none() {
                        active_tx = Some(engine.tx_store().begin());
                    }
                    out_buf.extend_from_slice(&PgMessageBuilder::command_complete("BEGIN"));
                } else if upper == "COMMIT" || upper.starts_with("COMMIT ") || upper == "END" {
                    if let Some(tx) = active_tx.take() {
                        let eng = Arc::clone(&engine);
                        let res: H2Result<()> = tokio::task::spawn_blocking(move || eng.commit_transaction(&tx)).await
                            .map_err(|e| H2Error::Execution(e.to_string()))?;
                        if let Err(e) = res {
                            out_buf.extend_from_slice(&PgMessageBuilder::error_response(&e.to_string()));
                        } else {
                            out_buf.extend_from_slice(&PgMessageBuilder::command_complete("COMMIT"));
                        }
                    } else {
                        out_buf.extend_from_slice(&PgMessageBuilder::command_complete("COMMIT"));
                    }
                } else if upper == "ROLLBACK" || upper.starts_with("ROLLBACK ") {
                    if let Some(tx) = active_tx.take() {
                        let _ = tokio::task::spawn_blocking(move || tx.rollback()).await;
                    }
                    out_buf.extend_from_slice(&PgMessageBuilder::command_complete("ROLLBACK"));
                } else if upper.starts_with("SET ") {
                    out_buf.extend_from_slice(&PgMessageBuilder::command_complete("SET"));
                } else if upper.starts_with("SHOW ") && upper != "SHOW QUERY STATS" {
                    let var = trimmed[5..].trim().to_lowercase();
                    let val = match var.as_str() {
                        "client_encoding" => "UTF8",
                        "server_version" => "15.0 (h2-rust)",
                        "standard_conforming_strings" => "on",
                        "transaction_isolation" | "default_transaction_isolation" => "read committed",
                        _ => "on",
                    };
                    out_buf.extend_from_slice(&PgMessageBuilder::row_description(&[var]));
                    out_buf.extend_from_slice(&PgMessageBuilder::data_row(&[h2_types::Value::String(val.to_string())]));
                    out_buf.extend_from_slice(&PgMessageBuilder::command_complete("SHOW"));
                } else if upper == "DISCARD ALL" || upper == "RESET ALL" {
                    out_buf.extend_from_slice(&PgMessageBuilder::command_complete("DISCARD"));
                } else if upper.starts_with("COPY ") && upper.contains(" FROM STDIN") {
                    let table_part = trimmed[5..].trim();
                    let target_table = table_part.split(|c: char| c.is_whitespace() || c == '(').next().unwrap_or("").trim();
                    let num_cols = if let Some(start) = table_part.find('(') {
                        if let Some(end) = table_part.find(')') {
                            table_part[start + 1..end].split(',').count()
                        } else {
                            5
                        }
                    } else {
                        5
                    };

                    stream.write_all(&PgMessageBuilder::copy_in_response(num_cols)).await?;
                    stream.flush().await?;

                    let mut copy_rows = Vec::new();
                    loop {
                        let mut c_type = [0u8; 1];
                        stream.read_exact(&mut c_type).await?;
                        let mut c_len_buf = [0u8; 4];
                        stream.read_exact(&mut c_len_buf).await?;
                        let c_len = (u32::from_be_bytes(c_len_buf) as usize).saturating_sub(4);
                        let mut c_payload = vec![0u8; c_len];
                        stream.read_exact(&mut c_payload).await?;

                        if c_type[0] == b'c' {
                            break; // CopyDone
                        } else if c_type[0] == b'd' {
                            let text = String::from_utf8_lossy(&c_payload);
                            for line in text.lines() {
                                let t = line.trim();
                                if !t.is_empty() && t != "\\." {
                                    copy_rows.push(t.to_string());
                                }
                            }
                        } else if c_type[0] == b'f' {
                            break; // CopyFail
                        }
                    }

                    let eng = Arc::clone(&engine);
                    let table_name = target_table.to_string();
                    let current_tx = active_tx.take();
                    let user_for_exec = username.clone();
                    let rows_to_insert = copy_rows.clone();

                    let (insert_res, returned_tx): (H2Result<usize>, Option<h2_mvstore::Transaction>) =
                        tokio::task::spawn_blocking(move || {
                            let tx_to_use = current_tx.unwrap_or_else(|| eng.tx_store().begin());
                            let mut count = 0;
                            for chunk in rows_to_insert.chunks(200) {
                                let mut val_tuples = Vec::new();
                                for r in chunk {
                                    let fields: Vec<String> = r.split(',').map(|f| {
                                        let tf = f.trim();
                                        if tf.is_empty() || tf.eq_ignore_ascii_case("null") {
                                            "NULL".to_string()
                                        } else if tf.starts_with('"') && tf.ends_with('"') {
                                            format!("'{}'", &tf[1..tf.len() - 1].replace('\'', "''"))
                                        } else if tf.parse::<f64>().is_ok() {
                                            tf.to_string()
                                        } else {
                                            format!("'{}'", tf.replace('\'', "''"))
                                        }
                                    }).collect();
                                    val_tuples.push(format!("({})", fields.join(", ")));
                                }
                                if !val_tuples.is_empty() {
                                    let sql = format!("INSERT INTO {} VALUES {}", table_name, val_tuples.join(", "));
                                    if let Err(e) = eng.execute_with_user_and_tx(&tx_to_use, &sql, Some(&user_for_exec)) {
                                        let _ = tx_to_use.rollback();
                                        return (Err(e), None);
                                    }
                                    count += chunk.len();
                                }
                            }
                            (Ok(count), Some(tx_to_use))
                        }).await.map_err(|e| H2Error::Execution(e.to_string()))?;

                    active_tx = returned_tx;
                    match insert_res {
                        Ok(cnt) => {
                            out_buf.extend_from_slice(&PgMessageBuilder::command_complete(&format!("COPY {}", cnt)));
                        }
                        Err(e) => {
                            out_buf.extend_from_slice(&PgMessageBuilder::error_response(&e.to_string()));
                        }
                    }
                } else {
                    let eng = Arc::clone(&engine);
                    let sql_owned = trimmed.to_string();
                    let current_tx = active_tx.take();
                    let user_for_exec = username.clone();

                    let (exec_result, returned_tx): (H2Result<ExecutionResult>, Option<h2_mvstore::Transaction>) =
                        tokio::task::spawn_blocking(move || {
                            let r = if let Some(ref tx) = current_tx {
                                eng.execute_with_user_and_tx(tx, &sql_owned, Some(&user_for_exec))
                            } else {
                                eng.execute_with_user(&sql_owned, Some(&user_for_exec))
                            };
                            (r, current_tx)
                        }).await.map_err(|e| H2Error::Execution(e.to_string()))?;

                    active_tx = returned_tx;

                    match exec_result {
                        Ok(result) => match result {
                            ExecutionResult::Ddl => {
                                let tag = if upper == "RESET QUERY STATS" {
                                    "RESET"
                                } else if upper.starts_with("CALL") {
                                    "CALL"
                                } else if upper.starts_with("CREATE FUNCTION") || upper.starts_with("CREATE OR REPLACE FUNCTION") {
                                    "CREATE FUNCTION"
                                } else if upper.starts_with("CREATE PROCEDURE") || upper.starts_with("CREATE OR REPLACE PROCEDURE") {
                                    "CREATE PROCEDURE"
                                } else if upper.starts_with("DROP") {
                                    "DROP"
                                } else {
                                    "CREATE TABLE"
                                };
                                out_buf.extend_from_slice(&PgMessageBuilder::command_complete(tag));
                            }
                            ExecutionResult::Dml { affected_rows } => {
                                let tag = if upper.starts_with("INSERT") {
                                    format!("INSERT 0 {}", affected_rows)
                                } else if upper.starts_with("UPDATE") {
                                    format!("UPDATE {}", affected_rows)
                                } else if upper.starts_with("DELETE") {
                                    format!("DELETE {}", affected_rows)
                                } else {
                                    format!("OK {}", affected_rows)
                                };
                                out_buf.extend_from_slice(&PgMessageBuilder::command_complete(&tag));
                            }
                            ExecutionResult::Query { columns, rows } => {
                                out_buf.extend_from_slice(&PgMessageBuilder::row_description(&columns));
                                for row in &rows {
                                    out_buf.extend_from_slice(&PgMessageBuilder::data_row(&row.values));
                                }
                                let tag = if upper.starts_with("CALL") {
                                    "CALL".to_string()
                                } else {
                                    format!("SELECT {}", rows.len())
                                };
                                out_buf.extend_from_slice(&PgMessageBuilder::command_complete(&tag));
                            }
                        },
                        Err(e) => {
                            out_buf.extend_from_slice(&PgMessageBuilder::error_response(&e.to_string()));
                        }
                    }
                }

                let tx_status = if active_tx.is_some() { b'T' } else { b'I' };
                out_buf.extend_from_slice(&PgMessageBuilder::ready_for_query(tx_status));
                stream.write_all(&out_buf).await?;
                stream.flush().await?;
            }
            b'X' => {
                // Terminate
                if let Some(tx) = active_tx.take() {
                    let _ = tokio::task::spawn_blocking(move || tx.rollback()).await;
                }
                break;
            }
            _ => {
                let tx_status = if active_tx.is_some() { b'T' } else { b'I' };
                stream.write_all(&PgMessageBuilder::ready_for_query(tx_status)).await?;
                stream.flush().await?;
            }
        }
    }

    Ok(())
}
