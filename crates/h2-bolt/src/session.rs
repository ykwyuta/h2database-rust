use std::collections::HashMap;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use h2_graph::{GraphEngine, GraphResult};
use h2_types::H2Result;

use crate::packstream::PackValue;

const BOLT_MAGIC: [u8; 4] = [0x60, 0x60, 0xB0, 0x17];
const BOLT_V5_0: [u8; 4] = [0, 0, 5, 0];
const BOLT_V4_4: [u8; 4] = [0, 0, 4, 4];

pub struct BoltSession {
    stream: TcpStream,
    engine: Arc<GraphEngine>,
    session_id: String,
    last_result: Option<GraphResult>,
}

impl BoltSession {
    pub fn new(stream: TcpStream, engine: Arc<GraphEngine>, session_id: impl Into<String>) -> Self {
        Self {
            stream,
            engine,
            session_id: session_id.into(),
            last_result: None,
        }
    }

    pub async fn run(&mut self) -> H2Result<()> {
        // 1. Handshake
        if !self.handle_handshake().await? {
            return Ok(());
        }

        // 2. Message Loop
        loop {
            let msg_bytes = match self.read_message().await? {
                Some(b) => b,
                None => break, // EOF
            };

            let (pv, _) = match PackValue::decode(&msg_bytes) {
                Ok(res) => res,
                Err(e) => {
                    self.send_failure("Neo.ClientError.Request.InvalidFormat", &e.to_string())
                        .await?;
                    continue;
                }
            };

            if let PackValue::Structure { tag, fields } = pv {
                let should_continue = self.handle_message(tag, fields).await?;
                if !should_continue {
                    break;
                }
            }
        }

        Ok(())
    }

    async fn handle_handshake(&mut self) -> H2Result<bool> {
        let mut magic = [0u8; 4];
        self.stream.read_exact(&mut magic).await?;

        if magic != BOLT_MAGIC {
            return Ok(false);
        }

        let mut versions = [0u8; 16];
        self.stream.read_exact(&mut versions).await?;

        // Check proposed versions
        let mut agreed_version = [0u8; 4];
        for i in 0..4 {
            let v = &versions[i * 4..(i + 1) * 4];
            let major = v[3];
            let minor = v[2];
            let range = v[0];

            if major == 5 {
                // Client proposed Bolt 5.x (standard big-endian format)
                agreed_version = [0, 0, 0, 5];
                break;
            } else if major == 4 && (minor == 4 || range >= minor.saturating_sub(4)) {
                // Client proposed Bolt 4.4 (standard big-endian format)
                agreed_version = [0, 0, 4, 4];
                break;
            } else if v == &BOLT_V5_0 {
                // Internal test representation [0, 0, 5, 0]
                agreed_version = BOLT_V5_0;
                break;
            } else if v == &BOLT_V4_4 {
                agreed_version = BOLT_V4_4;
                break;
            }
        }

        // If no matching version, default to v4.4 for broad driver compatibility
        if agreed_version == [0, 0, 0, 0] {
            agreed_version = [0, 0, 4, 4];
        }

        self.stream.write_all(&agreed_version).await?;
        self.stream.flush().await?;

        Ok(true)
    }

    async fn read_message(&mut self) -> H2Result<Option<Vec<u8>>> {
        let mut message_buf = Vec::new();

        loop {
            let mut chunk_size_bytes = [0u8; 2];
            match self.stream.read_exact(&mut chunk_size_bytes).await {
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                    return Ok(None);
                }
                Err(e) => return Err(e.into()),
            }

            let chunk_size = u16::from_be_bytes(chunk_size_bytes) as usize;
            if chunk_size == 0 {
                break; // End of message chunk stream
            }

            let mut chunk = vec![0u8; chunk_size];
            self.stream.read_exact(&mut chunk).await?;
            message_buf.extend_from_slice(&chunk);
        }

        Ok(Some(message_buf))
    }

    async fn write_message(&mut self, pv: &PackValue) -> H2Result<()> {
        let mut payload = Vec::new();
        pv.encode(&mut payload);

        // Chunk framing
        let mut offset = 0;
        while offset < payload.len() {
            let chunk_len = (payload.len() - offset).min(65535);
            let size_bytes = (chunk_len as u16).to_be_bytes();
            self.stream.write_all(&size_bytes).await?;
            self.stream
                .write_all(&payload[offset..offset + chunk_len])
                .await?;
            offset += chunk_len;
        }

        // End of message marker (0x00 0x00)
        self.stream.write_all(&[0x00, 0x00]).await?;
        self.stream.flush().await?;

        Ok(())
    }

    async fn handle_message(&mut self, tag: u8, fields: Vec<PackValue>) -> H2Result<bool> {
        match tag {
            // HELLO (0x01)
            0x01 => {
                let mut meta = HashMap::new();
                meta.insert(
                    "server".to_string(),
                    PackValue::String("Neo4j/5.0.0".to_string()),
                );
                meta.insert(
                    "connection_id".to_string(),
                    PackValue::String(self.session_id.clone()),
                );
                meta.insert("hints".to_string(), PackValue::Map(HashMap::new()));
                meta.insert("configuration_hints".to_string(), PackValue::Map(HashMap::new()));
                self.send_success(meta).await?;
                Ok(true)
            }

            // LOGON (0x6A)
            0x6A => {
                self.send_success(HashMap::new()).await?;
                Ok(true)
            }

            // RUN (0x10): fields: [statement: String, params: Map, extra: Map]
            0x10 => {
                let statement = match fields.first() {
                    Some(PackValue::String(s)) => s.clone(),
                    _ => {
                        self.send_failure(
                            "Neo.ClientError.Statement.SyntaxError",
                            "RUN statement must be a string",
                        )
                        .await?;
                        return Ok(true);
                    }
                };

                let mut params = HashMap::new();
                if let Some(PackValue::Map(ref pmap)) = fields.get(1) {
                    for (k, v) in pmap {
                        params.insert(k.clone(), v.to_graph_value());
                    }
                }

                match self.engine.execute_with_params(&statement, &params) {
                    Ok(result) => {
                        let mut meta = HashMap::new();
                        let cols: Vec<PackValue> = result
                            .columns
                            .iter()
                            .map(|c| PackValue::String(c.clone()))
                            .collect();
                        meta.insert("fields".to_string(), PackValue::List(cols));
                        meta.insert("t_first".to_string(), PackValue::Integer(0));
                        self.last_result = Some(result);
                        self.send_success(meta).await?;
                    }
                    Err(e) => {
                        self.send_failure("Neo.ClientError.Statement.SyntaxError", &e.to_string())
                            .await?;
                    }
                }
                Ok(true)
            }

            // PULL (0x3F): fields: [extra: Map]
            0x3F => {
                if let Some(result) = self.last_result.take() {
                    for row in result.rows {
                        let record_fields: Vec<PackValue> = row
                            .iter()
                            .map(PackValue::from_graph_value)
                            .collect();
                        // RECORD structure (tag 0x71)
                        let record = PackValue::Structure {
                            tag: 0x71,
                            fields: vec![PackValue::List(record_fields)],
                        };
                        self.write_message(&record).await?;
                    }

                    let mut meta = HashMap::new();
                    meta.insert("has_more".to_string(), PackValue::Boolean(false));

                    let mut stats_map = HashMap::new();
                    stats_map.insert(
                        "nodes-created".to_string(),
                        PackValue::Integer(result.stats.nodes_created as i64),
                    );
                    stats_map.insert(
                        "nodes-deleted".to_string(),
                        PackValue::Integer(result.stats.nodes_deleted as i64),
                    );
                    stats_map.insert(
                        "relationships-created".to_string(),
                        PackValue::Integer(result.stats.relationships_created as i64),
                    );
                    stats_map.insert(
                        "relationships-deleted".to_string(),
                        PackValue::Integer(result.stats.relationships_deleted as i64),
                    );
                    stats_map.insert(
                        "properties-set".to_string(),
                        PackValue::Integer(result.stats.properties_set as i64),
                    );
                    meta.insert(
                        "stats".to_string(),
                        PackValue::Map(stats_map),
                    );

                    self.send_success(meta).await?;
                } else {
                    let mut meta = HashMap::new();
                    meta.insert("has_more".to_string(), PackValue::Boolean(false));
                    self.send_success(meta).await?;
                }
                Ok(true)
            }

            // DISCARD (0x2F)
            0x2F => {
                self.last_result = None;
                let mut meta = HashMap::new();
                meta.insert("has_more".to_string(), PackValue::Boolean(false));
                self.send_success(meta).await?;
                Ok(true)
            }

            // BEGIN (0x11)
            0x11 => {
                self.send_success(HashMap::new()).await?;
                Ok(true)
            }

            // COMMIT (0x12)
            0x12 => {
                self.send_success(HashMap::new()).await?;
                Ok(true)
            }

            // ROLLBACK (0x13)
            0x13 => {
                self.send_success(HashMap::new()).await?;
                Ok(true)
            }

            // RESET (0x0F)
            0x0F => {
                self.last_result = None;
                self.send_success(HashMap::new()).await?;
                Ok(true)
            }

            // TELEMETRY (0x54)
            0x54 => {
                self.send_success(HashMap::new()).await?;
                Ok(true)
            }

            // ROUTE (0x66)
            0x66 => {
                let mut meta = HashMap::new();
                let mut rt = HashMap::new();
                rt.insert("ttl".to_string(), PackValue::Integer(300));

                let make_entry = |role: &str| {
                    let mut m = HashMap::new();
                    m.insert("role".to_string(), PackValue::String(role.to_string()));
                    m.insert(
                        "addresses".to_string(),
                        PackValue::List(vec![PackValue::String("127.0.0.1:7687".to_string())]),
                    );
                    PackValue::Map(m)
                };

                let servers = vec![
                    make_entry("ROUTE"),
                    make_entry("WRITE"),
                    make_entry("READ"),
                ];
                rt.insert("servers".to_string(), PackValue::List(servers));
                meta.insert("rt".to_string(), PackValue::Map(rt));
                self.send_success(meta).await?;
                Ok(true)
            }

            // GOODBYE (0x02)
            0x02 => {
                // Client gracefully closing
                Ok(false)
            }

            other => {
                self.send_failure(
                    "Neo.ClientError.Request.Invalid",
                    &format!("Unsupported message tag {other:#x}"),
                )
                .await?;
                Ok(true)
            }
        }
    }

    async fn send_success(&mut self, meta: HashMap<String, PackValue>) -> H2Result<()> {
        let msg = PackValue::Structure {
            tag: 0x70, // SUCCESS
            fields: vec![PackValue::Map(meta)],
        };
        self.write_message(&msg).await
    }

    async fn send_failure(&mut self, code: &str, message: &str) -> H2Result<()> {
        let mut meta = HashMap::new();
        meta.insert("code".to_string(), PackValue::String(code.to_string()));
        meta.insert(
            "message".to_string(),
            PackValue::String(message.to_string()),
        );
        let msg = PackValue::Structure {
            tag: 0x7F, // FAILURE
            fields: vec![PackValue::Map(meta)],
        };
        self.write_message(&msg).await
    }
}
