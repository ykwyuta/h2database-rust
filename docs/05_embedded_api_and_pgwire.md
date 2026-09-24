# 05. 組み込みAPI・インターフェース設計

## 1. 組み込み同期 API (rusqliteライクな体験)

開発者がSQLiteから移行する際に、違和感なく直感的に使えるよう `rusqlite` 風のエルゴノミクスを徹底します。

```rust
use h2::{Connection, Result, params};

fn main() -> Result<()> {
    // 1. ファイルデータベースを開く (存在しない場合は自動作成)
    //    インメモリの場合は Connection::open_in_memory()?
    let conn = Connection::open("my_database.h2")?;

    // 2. DDLの実行
    conn.execute(
        "CREATE TABLE users (
            id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
            name VARCHAR(100) NOT NULL,
            balance DECIMAL(15, 2) NOT NULL DEFAULT 0.00,
            embedding VECTOR(1536),
            created_at TIMESTAMP WITH TIME ZONE DEFAULT CURRENT_TIMESTAMP
        )",
        (),
    )?;

    // 3. パラメータ付きINSERT
    conn.execute(
        "INSERT INTO users (name, balance) VALUES (?1, ?2)",
        params!["Alice", "1500.50"],
    )?;

    // 4. トランザクション処理 (スナップショット分離)
    let tx = conn.transaction()?;
    tx.execute("UPDATE users SET balance = balance - 100 WHERE name = ?1", params!["Alice"])?;
    tx.commit()?;

    // 5. 型安全なクエリ走査
    let mut stmt = conn.prepare("SELECT name, balance FROM users WHERE balance > ?1")?;
    let user_iter = stmt.query_map(params![1000.0], |row| {
        Ok((
            row.get::<String, _>(0)?,
            row.get::<rust_decimal::Decimal, _>(1)?,
        ))
    })?;

    for user in user_iter {
        let (name, balance) = user?;
        println!("User: {name}, Balance: {balance}");
    }

    Ok(())
}
```

---

## 2. 非同期 API (Tokio ネイティブ)

モダンなRust Webフレームワーク（Axum, Actix-web, Tower等）向けに、非同期接続もネイティブサポートします。

```rust
use h2_async::{AsyncConnection, Result};

#[tokio::main]
async fn main() -> Result<()> {
    let conn = AsyncConnection::open("my_database.h2").await?;

    conn.execute("INSERT INTO logs (message) VALUES ($1)", &["Server started"]).await?;

    let rows = conn.query("SELECT message FROM logs", &[]).await?;
    for row in rows {
        let msg: String = row.get("message")?;
        println!("Log: {msg}");
    }

    Ok(())
}
```

---

## 3. PostgreSQL ワイヤプロトコルサーバー (内蔵 PG-Wire)

### なぜ PG-Wire なのか？
- **ツールエコシステムの享受**:
  - DBeaver, DataGrip, Navicat, TablePlus, VS Code Database Client, pgcli, psql など、世界中のあらゆるDBクライアントツールが無設定でそのまま接続可能になります。
- **マルチ言語からの利用**:
  - Python (psycopg2, asyncpg), Node.js (pg), Go (pgx), Java (JDBC Postgres Driver) から通常のリモートDBとして接続可能。
- **デバッグの容易性**:
  - アプリケーションが組み込みDBとしてローカルファイルで実行されている最中に、別ウィンドウの `psql` や `DBeaver` から同時に接続して中身をリアルタイムに確認可能。

### 設定と起動（組み込み時）
アプリコード内から1行でバックグラウンドサーバーを同時起動できます。

```rust
use h2::{Connection, ServerConfig};

fn main() -> anyhow::Result<()> {
    let conn = Connection::open("my_database.h2")?;

    // オプションでPostgres互換サーバーをバックグラウンド起動 (ポート5432)
    let _server = conn.start_pg_server(ServerConfig {
        bind_addr: "127.0.0.1:5432".parse()?,
        auth_mode: h2::AuthMode::Trust, // または Password / MD5 / SCRAM
    })?;

    println!("App running with embedded DB. Connect via: psql -h 127.0.0.1 -p 5432 -d my_database");

    // アプリケーション本来の処理...
    Ok(())
}
```

### サポートする PG-Wire メッセージ
- **StartupMessage / AuthenticationOk**: 接続確立と認証
- **Simple Query (`'Q'`)**: `SELECT 1;` などの単一クエリ文字列の実行と `RowDescription` / `DataRow` / `CommandComplete` 返却
- **Extended Query Protocol (`'P'`, `'B'`, `'E'`, `'S'`)**:
  - Parse, Bind, Execute, Sync によるプリペアドステートメントとパラメータバインドのバイナリ送受信
  - ORM (SQLx, Diesel, Prisma, Hibernate) との互換性確保
