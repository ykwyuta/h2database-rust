# H2 Database in Rust - 実践デモ集 (Demos)

本ディレクトリには、`h2database-rust` の主要な利用シナリオごとに独立して実行・検証できるデモプロジェクトが収録されています。

---

## 📂 デモ一覧

| ディレクトリ | ユースケース / 対象 | 主な技術スタック | 概要 |
| :--- | :--- | :--- | :--- |
| [**demo/001 専用CLI**](./001_cli/README.md) | 専用対話型シェル | `h2-cli`, REPL, SQLスクリプト | 組み込みシェルを起動し、対話的またはファイル経由で直接 SQL や日本語 FTS を実行するデモ |
| [**demo/002 psqlコマンド**](./002_psql/README.md) | PostgreSQL ツール接続 | PG-Wire, `psql`, DBeaver | バックグラウンドで PostgreSQL 互換サーバーを起動し、標準の `psql` コマンドや GUI ツールから操作するデモ |
| [**demo/003 組み込み利用（同期）**](./003_embedded_sync/README.md) | Rust 組み込み同期 API | `Connection`, `params!`, `Row::get_as` | SQLite（rusqlite）のように Rust アプリ内に直接組み込み、型安全な CRUD・トランザクション・FTS を実行するデモ |
| [**demo/004 組み込み利用（非同期）**](./004_embedded_async/README.md) | Rust 組み込み非同期 API | `AsyncConnection`, Tokio, 並行タスク | Tokio ランタイムを用いてイベントループをブロックせず、Web サーバー（Axum 等）で並行クエリを実行するデモ |
| [**demo/005 Java JDBCドライバ**](./005_jdbc/README.md) | Java / JVM エコシステム | Java 17+, JDBC, `org.postgresql.Driver` | 公式 PostgreSQL JDBC ドライバ経由で Java プログラムから接続し、PreparedStatement やトランザクションを操作するデモ |

---

## 🏃 クイック実行コマンド一覧

```bash
# 1. 専用CLIの起動
cargo run -p h2-cli -- demo.h2

# 2. PostgreSQL 互換サーバーの起動 (psql接続用)
cargo run -p demo-psql-server

# 3. Rust 組み込み同期デモの実行
cargo run -p demo-embedded-sync

# 4. Rust 組み込み非同期デモの実行
cargo run -p demo-embedded-async

# 5. Java JDBC デモの実行 (サーバー起動後)
cd demo/005_jdbc && mvn compile exec:java
```
