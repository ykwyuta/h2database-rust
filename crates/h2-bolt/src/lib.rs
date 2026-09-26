pub mod packstream;
pub mod session;
pub mod server;

pub use packstream::PackValue;
pub use session::BoltSession;
pub use server::BoltServer;

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;
    use tokio::sync::broadcast;

    use h2_graph::GraphEngine;
    use h2_mvstore::MVStore;

    use crate::packstream::PackValue;
    use crate::server::BoltServer;

    #[test]
    fn test_packstream_roundtrip() {
        // 1. Primitive types
        let vals = vec![
            PackValue::Null,
            PackValue::Boolean(true),
            PackValue::Boolean(false),
            PackValue::Integer(0),
            PackValue::Integer(42),
            PackValue::Integer(-15),
            PackValue::Integer(1000),
            PackValue::Integer(-50000),
            PackValue::Integer(1000000000),
            PackValue::Float(3.14159),
            PackValue::String("hello bolt".to_string()),
        ];

        for val in vals {
            let mut buf = Vec::new();
            val.encode(&mut buf);
            let (decoded, consumed) = PackValue::decode(&buf).unwrap();
            assert_eq!(consumed, buf.len());
            assert_eq!(decoded, val);
        }

        // 2. Map and List
        let mut map = HashMap::new();
        map.insert("name".to_string(), PackValue::String("Alice".to_string()));
        map.insert("score".to_string(), PackValue::Integer(100));

        let complex = PackValue::List(vec![
            PackValue::Integer(1),
            PackValue::String("item".to_string()),
            PackValue::Map(map),
        ]);

        let mut buf = Vec::new();
        complex.encode(&mut buf);
        let (decoded, consumed) = PackValue::decode(&buf).unwrap();
        assert_eq!(consumed, buf.len());
        assert_eq!(decoded, complex);
    }

    #[tokio::test]
    async fn test_bolt_server_full_flow() {
        // 1. Setup in-memory GraphEngine and BoltServer
        let mvstore = Arc::new(MVStore::open_in_memory());
        let engine = Arc::new(GraphEngine::new(mvstore, "bolt_test_graph").unwrap());

        let server = BoltServer::bind("127.0.0.1:0", engine).await.unwrap();
        let addr = server.local_addr().unwrap();

        let (shutdown_tx, shutdown_rx) = broadcast::channel(1);
        let server_handle = tokio::spawn(async move {
            server.run(shutdown_rx).await.unwrap();
        });

        // 2. Connect client TCP stream
        let mut stream = TcpStream::connect(addr).await.unwrap();

        // 3. Handshake: Magic + 4 proposed versions
        let magic = [0x60, 0x60, 0xB0, 0x17];
        let v5_0 = [0, 0, 5, 0];
        let v4_4 = [0, 0, 4, 4];
        let zero = [0, 0, 0, 0];

        let mut handshake_bytes = Vec::new();
        handshake_bytes.extend_from_slice(&magic);
        handshake_bytes.extend_from_slice(&v5_0);
        handshake_bytes.extend_from_slice(&v4_4);
        handshake_bytes.extend_from_slice(&zero);
        handshake_bytes.extend_from_slice(&zero);

        stream.write_all(&handshake_bytes).await.unwrap();
        stream.flush().await.unwrap();

        let mut agreed_version = [0u8; 4];
        stream.read_exact(&mut agreed_version).await.unwrap();
        assert_eq!(agreed_version, v5_0);

        // Helper to send a framed PackStream message
        async fn send_msg(stream: &mut TcpStream, pv: &PackValue) {
            let mut payload = Vec::new();
            pv.encode(&mut payload);
            let len_bytes = (payload.len() as u16).to_be_bytes();
            stream.write_all(&len_bytes).await.unwrap();
            stream.write_all(&payload).await.unwrap();
            stream.write_all(&[0x00, 0x00]).await.unwrap(); // end marker
            stream.flush().await.unwrap();
        }

        // Helper to read a framed PackStream message
        async fn read_msg(stream: &mut TcpStream) -> PackValue {
            let mut buf = Vec::new();
            loop {
                let mut chunk_size = [0u8; 2];
                stream.read_exact(&mut chunk_size).await.unwrap();
                let len = u16::from_be_bytes(chunk_size) as usize;
                if len == 0 {
                    break;
                }
                let mut chunk = vec![0u8; len];
                stream.read_exact(&mut chunk).await.unwrap();
                buf.extend_from_slice(&chunk);
            }
            let (pv, _) = PackValue::decode(&buf).unwrap();
            pv
        }

        // 4. Send HELLO (tag 0x01)
        let hello = PackValue::Structure {
            tag: 0x01,
            fields: vec![PackValue::Map(HashMap::new())],
        };
        send_msg(&mut stream, &hello).await;

        let resp = read_msg(&mut stream).await;
        if let PackValue::Structure { tag, fields } = resp {
            assert_eq!(tag, 0x70); // SUCCESS
            assert!(!fields.is_empty());
        } else {
            panic!("Expected SUCCESS structure, got {:?}", resp);
        }

        // 5. Send RUN (tag 0x10) to create and return node
        let cypher = "CREATE (a:Hero {name: 'Neo', power: 9000}) RETURN a.name, a.power";
        let run_msg = PackValue::Structure {
            tag: 0x10,
            fields: vec![
                PackValue::String(cypher.to_string()),
                PackValue::Map(HashMap::new()),
                PackValue::Map(HashMap::new()),
            ],
        };
        send_msg(&mut stream, &run_msg).await;

        let run_resp = read_msg(&mut stream).await;
        if let PackValue::Structure { tag, fields } = run_resp {
            assert_eq!(tag, 0x70); // SUCCESS
            if let PackValue::Map(meta) = &fields[0] {
                if let Some(PackValue::List(cols)) = meta.get("fields") {
                    assert_eq!(cols.len(), 2);
                    assert_eq!(cols[0], PackValue::String("a.name".to_string()));
                    assert_eq!(cols[1], PackValue::String("a.power".to_string()));
                } else {
                    panic!("Missing fields in RUN SUCCESS meta");
                }
            }
        } else {
            panic!("Expected RUN SUCCESS, got {:?}", run_resp);
        }

        // 6. Send PULL (tag 0x3F) to fetch records
        let pull_msg = PackValue::Structure {
            tag: 0x3F,
            fields: vec![PackValue::Map(HashMap::new())],
        };
        send_msg(&mut stream, &pull_msg).await;

        // Expect RECORD (tag 0x71)
        let rec = read_msg(&mut stream).await;
        if let PackValue::Structure { tag, fields } = rec {
            assert_eq!(tag, 0x71); // RECORD
            if let PackValue::List(row) = &fields[0] {
                assert_eq!(row[0], PackValue::String("Neo".to_string()));
                assert_eq!(row[1], PackValue::Integer(9000));
            } else {
                panic!("Invalid record fields: {:?}", fields);
            }
        } else {
            panic!("Expected RECORD, got {:?}", rec);
        }

        // Expect PULL SUCCESS (tag 0x70)
        let pull_success = read_msg(&mut stream).await;
        if let PackValue::Structure { tag, fields } = pull_success {
            assert_eq!(tag, 0x70);
            if let PackValue::Map(meta) = &fields[0] {
                assert_eq!(meta.get("has_more"), Some(&PackValue::Boolean(false)));
            }
        } else {
            panic!("Expected PULL SUCCESS, got {:?}", pull_success);
        }

        // 7. Send GOODBYE (tag 0x02)
        let goodbye = PackValue::Structure {
            tag: 0x02,
            fields: vec![],
        };
        send_msg(&mut stream, &goodbye).await;

        // Shutdown server
        let _ = shutdown_tx.send(());
        let _ = server_handle.await;
    }
}
