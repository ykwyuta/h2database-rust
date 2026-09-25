use h2::{Connection, H2Error};
use tokio::net::TcpStream;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[test]
fn test_user_management_ddl() {
    let conn = Connection::open_in_memory().unwrap();

    // 1. ユーザー作成
    conn.execute("CREATE USER 'alice' PASSWORD 'alice123' HOST 'localhost'").unwrap();
    conn.execute("CREATE USER 'bob' PASSWORD 'bob123' HOST '192.168.1.0/24'").unwrap();

    // 2. SHOW USERS で確認
    let users = conn.query("SHOW USERS").unwrap();
    let usernames: Vec<String> = users.iter().map(|r| r.get_as::<String>(0).unwrap()).collect();
    assert!(usernames.contains(&"alice".to_string()));
    assert!(usernames.contains(&"bob".to_string()));

    // 3. ALTER USER
    conn.execute("ALTER USER 'alice' PASSWORD 'newpass456' HOST '%'").unwrap();

    // 4. DROP USER
    conn.execute("DROP USER 'bob'").unwrap();
    let users2 = conn.query("SHOW USERS").unwrap();
    let usernames2: Vec<String> = users2.iter().map(|r| r.get_as::<String>(0).unwrap()).collect();
    assert!(usernames2.contains(&"alice".to_string()));
    assert!(!usernames2.contains(&"bob".to_string()));
}

#[test]
fn test_host_restriction_and_password_auth() {
    let conn = Connection::open_in_memory().unwrap();

    conn.execute("CREATE USER 'local_user' PASSWORD 'secret' HOST '127.0.0.1'").unwrap();
    conn.execute("CREATE USER 'subnet_user' PASSWORD 'secret' HOST '192.168.10.0/24'").unwrap();
    conn.execute("CREATE USER 'any_user' PASSWORD 'secret' HOST '%'").unwrap();

    // local_user: 127.0.0.1 から成功
    assert!(conn.authenticate("local_user", Some("secret"), "127.0.0.1").is_ok());
    // local_user: 異なるホストから拒否
    let err = conn.authenticate("local_user", Some("secret"), "10.0.0.1").unwrap_err();
    assert!(matches!(err, H2Error::Authentication(_)));
    // local_user: パスワード不一致
    let err = conn.authenticate("local_user", Some("wrong"), "127.0.0.1").unwrap_err();
    assert!(matches!(err, H2Error::Authentication(_)));

    // subnet_user: 同一CIDR (192.168.10.50) から成功
    assert!(conn.authenticate("subnet_user", Some("secret"), "192.168.10.50").is_ok());
    // subnet_user: 別CIDR (192.168.20.50) から拒否
    let err = conn.authenticate("subnet_user", Some("secret"), "192.168.20.50").unwrap_err();
    assert!(matches!(err, H2Error::Authentication(_)));

    // any_user: 任意のIPから成功
    assert!(conn.authenticate("any_user", Some("secret"), "172.16.0.99").is_ok());
}

#[test]
fn test_table_level_authorization_and_grants() {
    let admin_conn = Connection::open_in_memory().unwrap();

    // 管理者としてテーブル作成＆データ投入
    admin_conn.execute("CREATE TABLE sales (id INT PRIMARY KEY, amount INT)").unwrap();
    admin_conn.execute("CREATE TABLE payroll (id INT PRIMARY KEY, salary INT)").unwrap();
    admin_conn.execute("INSERT INTO sales VALUES (1, 100), (2, 200)").unwrap();
    admin_conn.execute("INSERT INTO payroll VALUES (1, 5000)").unwrap();

    // 一般ユーザー developer を作成
    admin_conn.execute("CREATE USER 'developer' PASSWORD 'devpass' HOST '%'").unwrap();

    // developer セッション
    let dev_conn = admin_conn.new_session();
    dev_conn.set_current_user(Some("developer"));

    // 権限なしの状態: sales も payroll も SELECT 拒否
    let err = dev_conn.query("SELECT * FROM sales").unwrap_err();
    assert!(matches!(err, H2Error::PermissionDenied(_)));

    let err = dev_conn.execute("INSERT INTO sales VALUES (3, 300)").unwrap_err();
    assert!(matches!(err, H2Error::PermissionDenied(_)));

    // 管理者が sales に対する SELECT, INSERT を付与
    admin_conn.execute("GRANT SELECT, INSERT ON TABLE sales TO 'developer'").unwrap();

    // SHOW GRANTS 確認
    let grants = admin_conn.query("SHOW GRANTS FOR 'developer'").unwrap();
    assert!(!grants.is_empty());

    // developer: sales の SELECT は成功
    let rows = dev_conn.query("SELECT * FROM sales").unwrap();
    assert_eq!(rows.len(), 2);

    // developer: sales の INSERT は成功
    let affected = dev_conn.execute("INSERT INTO sales VALUES (3, 300)").unwrap();
    assert_eq!(affected, 1);

    // developer: sales の UPDATE / DELETE は権限なしで拒否
    let err = dev_conn.execute("UPDATE sales SET amount = 999 WHERE id = 1").unwrap_err();
    assert!(matches!(err, H2Error::PermissionDenied(_)));

    let err = dev_conn.execute("DELETE FROM sales WHERE id = 1").unwrap_err();
    assert!(matches!(err, H2Error::PermissionDenied(_)));

    // developer: payroll に対するアクセスは依然として拒否
    let err = dev_conn.query("SELECT * FROM payroll").unwrap_err();
    assert!(matches!(err, H2Error::PermissionDenied(_)));

    // developer: DDL (CREATE TABLE) の実行は一般ユーザーのため拒否
    let err = dev_conn.execute("CREATE TABLE test_tbl (id INT)").unwrap_err();
    assert!(matches!(err, H2Error::PermissionDenied(_)));

    // 管理者が INSERT 権限を剥奪 (REVOKE)
    admin_conn.execute("REVOKE INSERT ON TABLE sales FROM 'developer'").unwrap();

    // developer: sales の INSERT が拒否されることを確認
    let err = dev_conn.execute("INSERT INTO sales VALUES (4, 400)").unwrap_err();
    assert!(matches!(err, H2Error::PermissionDenied(_)));

    // developer: sales の SELECT は依然として成功することを確認
    let rows = dev_conn.query("SELECT * FROM sales").unwrap();
    assert_eq!(rows.len(), 3);
}

#[tokio::test]
async fn test_pgwire_authentication_handshake() {
    let conn = Connection::open_in_memory().unwrap();

    // ユーザー作成: app_user (HOST=127.0.0.1, PASSWORD=app_secret)
    conn.execute("CREATE USER 'app_user' PASSWORD 'app_secret' HOST '127.0.0.1'").unwrap();
    conn.execute("CREATE TABLE test_data (id INT PRIMARY KEY, name VARCHAR(50))").unwrap();
    conn.execute("INSERT INTO test_data VALUES (1, 'hello')").unwrap();
    conn.execute("GRANT SELECT ON TABLE test_data TO 'app_user'").unwrap();

    let server_addr = conn
        .start_pg_server("127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();

    // 1. 正しいパスワードでの接続
    let mut client = TcpStream::connect(server_addr).await.unwrap();

    // SSLRequest
    let mut ssl_req = Vec::new();
    ssl_req.extend_from_slice(&8u32.to_be_bytes());
    ssl_req.extend_from_slice(&80877103u32.to_be_bytes());
    client.write_all(&ssl_req).await.unwrap();
    let mut ssl_resp = [0u8; 1];
    client.read_exact(&mut ssl_resp).await.unwrap();

    // StartupMessage with user = app_user
    let mut startup = Vec::new();
    startup.extend_from_slice(&196608u32.to_be_bytes());
    startup.extend_from_slice(b"user\0app_user\0database\0main\0\0");
    let startup_len = 4 + startup.len() as u32;

    let mut startup_msg = Vec::new();
    startup_msg.extend_from_slice(&startup_len.to_be_bytes());
    startup_msg.extend_from_slice(&startup);
    client.write_all(&startup_msg).await.unwrap();

    // サーバーから AuthenticationCleartextPassword ('R', len 8, code 3) を受信
    let mut auth_req = [0u8; 9];
    client.read_exact(&mut auth_req).await.unwrap();
    assert_eq!(auth_req[0], b'R');
    let auth_type = u32::from_be_bytes(auth_req[5..9].try_into().unwrap());
    assert_eq!(auth_type, 3); // CleartextPassword

    // パスワード送信 ('p')
    let pwd = b"app_secret\0";
    let pwd_len = 4 + pwd.len() as u32;
    let mut pwd_msg = Vec::new();
    pwd_msg.push(b'p');
    pwd_msg.extend_from_slice(&pwd_len.to_be_bytes());
    pwd_msg.extend_from_slice(pwd);
    client.write_all(&pwd_msg).await.unwrap();

    // 認証成功レスポンス等を読み取り
    let mut buf = vec![0u8; 1024];
    let n = client.read(&mut buf).await.unwrap();
    assert!(n > 0);

    // クエリ実行: SELECT は GRANT されているので成功
    let sql = "SELECT * FROM test_data";
    let sql_bytes = sql.as_bytes();
    let len = 4 + sql_bytes.len() + 1;
    let mut query_buf = Vec::new();
    query_buf.push(b'Q');
    query_buf.extend_from_slice(&(len as u32).to_be_bytes());
    query_buf.extend_from_slice(sql_bytes);
    query_buf.push(0);
    client.write_all(&query_buf).await.unwrap();

    let mut resp_buf = vec![0u8; 1024];
    let n = client.read(&mut resp_buf).await.unwrap();
    let resp_str = String::from_utf8_lossy(&resp_buf[..n]);
    assert!(resp_str.contains("hello"));

    // クエリ実行: INSERT は権限がないので Permission denied エラーが返ること
    let bad_sql = "INSERT INTO test_data VALUES (2, 'world')";
    let bad_bytes = bad_sql.as_bytes();
    let len = 4 + bad_bytes.len() + 1;
    let mut bad_buf = Vec::new();
    bad_buf.push(b'Q');
    bad_buf.extend_from_slice(&(len as u32).to_be_bytes());
    bad_buf.extend_from_slice(bad_bytes);
    bad_buf.push(0);
    client.write_all(&bad_buf).await.unwrap();

    let n = client.read(&mut resp_buf).await.unwrap();
    let err_str = String::from_utf8_lossy(&resp_buf[..n]);
    assert!(err_str.contains("Permission denied") || resp_buf[0] == b'E');
}
