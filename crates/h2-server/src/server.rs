use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tracing::{error, info};

use h2_sql::{ExecutionResult, SQLEngine};
use h2_types::H2Result;
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
                if let Err(e) = handle_client(socket, engine).await {
                    error!("Error handling client {}: {:?}", client_addr, e);
                }
            });
        }
    }
}

async fn handle_client(mut stream: TcpStream, engine: Arc<SQLEngine>) -> H2Result<()> {
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
    if msg_len > 8 {
        let mut rest_buf = vec![0u8; msg_len - 8];
        stream.read_exact(&mut rest_buf).await?;
    }

    // 認証成功と初期パラメータ、ReadyForQuery を送信
    stream.write_all(&PgMessageBuilder::authentication_ok()).await?;
    stream.write_all(&PgMessageBuilder::parameter_status("server_version", "15.0 (h2-rust)")).await?;
    stream.write_all(&PgMessageBuilder::parameter_status("client_encoding", "UTF8")).await?;
    stream.write_all(&PgMessageBuilder::ready_for_query(b'I')).await?;
    stream.flush().await?;

    // 2. コマンド処理ループ
    let mut type_buf = [0u8; 1];
    loop {
        let read_bytes = stream.read(&mut type_buf).await?;
        if read_bytes == 0 {
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

                if sql.is_empty() || sql == ";" {
                    stream.write_all(&PgMessageBuilder::command_complete("")).await?;
                    stream.write_all(&PgMessageBuilder::ready_for_query(b'I')).await?;
                    stream.flush().await?;
                    continue;
                }

                match engine.execute(&sql) {
                    Ok(result) => match result {
                        ExecutionResult::Ddl => {
                            stream.write_all(&PgMessageBuilder::command_complete("CREATE TABLE")).await?;
                        }
                        ExecutionResult::Dml { affected_rows } => {
                            let tag = format!("INSERT 0 {}", affected_rows);
                            stream.write_all(&PgMessageBuilder::command_complete(&tag)).await?;
                        }
                        ExecutionResult::Query { columns, rows } => {
                            stream.write_all(&PgMessageBuilder::row_description(&columns)).await?;
                            for row in &rows {
                                stream.write_all(&PgMessageBuilder::data_row(&row.values)).await?;
                            }
                            let tag = format!("SELECT {}", rows.len());
                            stream.write_all(&PgMessageBuilder::command_complete(&tag)).await?;
                        }
                    },
                    Err(e) => {
                        stream.write_all(&PgMessageBuilder::error_response(&e.to_string())).await?;
                    }
                }

                stream.write_all(&PgMessageBuilder::ready_for_query(b'I')).await?;
                stream.flush().await?;
            }
            b'X' => {
                // Terminate
                break;
            }
            _ => {
                // その他のメッセージは一旦 ReadyForQuery を返してスキップ
                stream.write_all(&PgMessageBuilder::ready_for_query(b'I')).await?;
                stream.flush().await?;
            }
        }
    }

    Ok(())
}
