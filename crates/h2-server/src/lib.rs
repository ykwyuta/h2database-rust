pub mod protocol;
pub mod server;

pub use protocol::{PgMessageBuilder, StartupMessage};
pub use server::PgServer;

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    use h2_mvstore::MVStore;
    use h2_sql::SQLEngine;

    #[tokio::test]
    async fn test_pgwire_server_handshake_and_query() {
        let store = Arc::new(MVStore::open_in_memory());
        let engine = Arc::new(SQLEngine::new(store).unwrap());

        let server = PgServer::bind("127.0.0.1:0".parse().unwrap(), Arc::clone(&engine))
            .await
            .unwrap();
        let addr = server.local_addr().unwrap();

        // サーバーをバックグラウンド実行
        tokio::spawn(async move {
            let _ = server.run().await;
        });

        // クライアント接続
        let mut client = TcpStream::connect(addr).await.unwrap();

        // 1. SSLRequest を送信 (8バイト: len=8, code=80877103)
        let mut ssl_req = Vec::new();
        ssl_req.extend_from_slice(&8u32.to_be_bytes());
        ssl_req.extend_from_slice(&protocol::SSL_REQUEST_CODE.to_be_bytes());
        client.write_all(&ssl_req).await.unwrap();

        // サーバーから 'N' を受信
        let mut ssl_resp = [0u8; 1];
        client.read_exact(&mut ssl_resp).await.unwrap();
        assert_eq!(ssl_resp[0], b'N');

        // 2. StartupMessage を送信
        let mut startup = Vec::new();
        startup.extend_from_slice(&protocol::PROTOCOL_VERSION_3.to_be_bytes());
        startup.extend_from_slice(b"user\0postgres\0database\0testdb\0\0");
        let startup_len = 4 + startup.len() as u32;

        let mut startup_msg = Vec::new();
        startup_msg.extend_from_slice(&startup_len.to_be_bytes());
        startup_msg.extend_from_slice(&startup);
        client.write_all(&startup_msg).await.unwrap();

        // サーバーからの初期応答 (AuthOK, ParameterStatus x 2, ReadyForQuery) を読み取り
        let mut init_buf = vec![0u8; 1024];
        let bytes_read = client.read(&mut init_buf).await.unwrap();
        assert!(bytes_read > 0);
        // 先頭は 'R' (AuthenticationOk)
        assert_eq!(init_buf[0], b'R');

        // 3. Simple Query ('Q'): CREATE TABLE
        send_query(&mut client, "CREATE TABLE items (id INTEGER, name VARCHAR)").await;
        let mut resp_buf = vec![0u8; 512];
        let _n = client.read(&mut resp_buf).await.unwrap();
        assert_eq!(resp_buf[0], b'C'); // CommandComplete

        // 4. Simple Query ('Q'): INSERT
        send_query(&mut client, "INSERT INTO items VALUES (1, 'Widget')").await;
        let _n = client.read(&mut resp_buf).await.unwrap();
        assert_eq!(resp_buf[0], b'C'); // CommandComplete

        // 5. Simple Query ('Q'): SELECT
        send_query(&mut client, "SELECT id, name FROM items").await;
        let n = client.read(&mut resp_buf).await.unwrap();
        assert_eq!(resp_buf[0], b'T'); // RowDescription
        // メッセージ中に 'Widget' が含まれていることを確認
        let resp_str = String::from_utf8_lossy(&resp_buf[..n]);
        assert!(resp_str.contains("Widget"));
    }

    async fn send_query(stream: &mut TcpStream, sql: &str) {
        let sql_bytes = sql.as_bytes();
        let len = 4 + sql_bytes.len() + 1;
        let mut buf = Vec::new();
        buf.push(b'Q');
        buf.extend_from_slice(&(len as u32).to_be_bytes());
        buf.extend_from_slice(sql_bytes);
        buf.push(0);
        stream.write_all(&buf).await.unwrap();
        stream.flush().await.unwrap();
    }
}
