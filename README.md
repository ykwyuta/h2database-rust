# h2database-rust (h2-rust)

[![Rust](https://img.shields.io/badge/rust-1.80%2B-orange.svg)](https://www.rust-lang.org)
[![License](https://img.shields.io/badge/license-MPL--2.0%20OR%20Apache--2.0-blue.svg)](LICENSE)

**SQLiteのように手軽に組み込めるが、SQLiteよりも圧倒的に高機能かつ並行性に優れた次世代組み込み／サーバーハイブリッドRDBMS**

Java版 [H2 Database](https://github.com/h2database/h2database) の先進的なログ構造化 CoW (Copy-on-Write) B-Tree ストレージエンジン（**MVStore**）および PostgreSQL 互換ワイヤプロトコルの思想を受け継ぎ、Rust ネイティブでゼロから再構築されたリレーショナルデータベースです。

---

## 🌟 特長

- **🚀 組み込みファースト (Zero-Config, Single Library)**:
  - 外部プロセスの起動や設定ファイルは不要。`h2::Connection::open("mydb.h2")?` または `h2::Connection::open_in_memory()?` の1行で利用可能。
- **⚡ MVCC による高い並行書き込み性能**:
  - SQLiteのシングルライター制約（WAL時でも同時書き込みトランザクションは1つのみ）を打破。CoW B-Tree によるスナップショット分離と高速コミットを実現。
- **📊 高度なSQL実行エンジン & インデックス**:
  - `UPDATE`（複数列更新・自己参照式計算）、`ORDER BY` / `LIMIT` / `OFFSET`（昇順降順・エイリアス対応）、集約関数（`COUNT`, `SUM`, `AVG`, `MIN`, `MAX`）＆ `GROUP BY` / `HAVING`、テーブル結合（`INNER JOIN` / `LEFT OUTER JOIN`）。
  - セカンダリインデックス（`CREATE INDEX`, `CREATE UNIQUE INDEX`）、一意性制約の実行時バリデーション、**IndexScan** 高速化。
- **📦 JSONB / ネイティブ JSON ＆ 抽出オペレータ**:
  - `JSON` 型への保存とバリデーション、`JSON_EXTRACT` 関数、`->`（JSON抽出）および `->>`（テキスト抽出）演算子を標準サポート。
- **🛡️ 厳格でモダンな型システム & パラメータ化クエリ**:
  - 動的型付け（Type Affinity）による不整合を防止。任意精度 `Decimal`、タイムゾーン付き `Timestamp`、ネイティブ `UUID`、`JSON` を完備。
  - `params![...]` マクロおよび `execute_params` / `query_params` による SQL インジェクション防止と型安全なバインド。
- **🔍 日本語対応 N-Gram & 形態素解析 全文検索 (FTS)**:
  - 外部拡張不要で、N-Gram（バイグラム）および文字種境界解析による形態素トークナイザーを内蔵。日本語テキストのインデックス作成と高速な全文検索をサポート。
- **🔌 内蔵 PostgreSQL 互換ワイヤプロトコル (PG-Wire) & INFORMATION_SCHEMA**:
  - アプリ内で組み込み動作させながら、オプションでポート5432を開放。稼働中のアプリを停止させずに **DBeaver, DataGrip, VS Code拡張, psql** から直接クエリを発行してデバッグ・管理可能。
  - `information_schema.tables`, `information_schema.columns` システムビューを提供。
- **⚡ Tokio ネイティブ非同期 API (`AsyncConnection`)**:
  - `AsyncConnection::open(path).await` により Axum や Actix-web 等の非同期 Web サービスにそのまま統合可能。

---

## 📚 ドキュメント

- 📖 **[利用者向け公式ガイド (User's Guide)](./docs/USER_GUIDE.md)**:
  PostgreSQL 公式ドキュメント構成をベースにした網羅的ガイド。データ型、SQL構文、トランザクション、日本語全文検索、非同期API、DBeaver接続手順までを解説。
- 🏛️ **アーキテクチャ設計・仕様ドキュメント**:
  1. [プロジェクトビジョンと技術比較](./docs/01_overview_and_vision.md)
  2. [全体アーキテクチャ設計](./docs/02_architecture_overview.md)
  3. [ストレージエンジン設計（Rust版 MVStore）](./docs/03_storage_engine_mvstore.md)
  4. [SQL処理系・型システム・実行エンジン](./docs/04_sql_parser_and_execution.md)
  5. [組み込みAPI・インターフェース設計](./docs/05_embedded_api_and_pgwire.md)
  6. [実装ロードマップとマイルストーン](./docs/06_roadmap_and_phases.md)

---

## 📦 クレート構成

```text
h2database-rust/
├── crates/
│   ├── h2-types/       # 基本データ型 (DataType, Value) およびエラー定義
│   ├── h2-mvstore/     # 追記型 CoW B-Tree ＆ MVCC ストレージエンジン
│   ├── h2-sql/         # SQLパーサ、カタログ、クエリオプティマイザ、実行エンジン
│   ├── h2-server/      # PostgreSQL v3 プロトコルサーバー
│   ├── h2-cli/         # 対話型シェル REPL
│   └── h2/             # ユーザー向け最上位ファサードクレート (組み込みAPI)
└── docs/               # 設計・仕様ドキュメント
```

---

## 🛠️ クイックスタート

### 1. 同期 組み込みモード

```rust
use h2::{Connection, H2Result, params};
use rust_decimal::Decimal;
use std::str::FromStr;

fn main() -> H2Result<()> {
    // データベースを開く（またはインメモリ: open_in_memory()）
    let conn = Connection::open("test.h2")?;

    // テーブルの作成
    conn.execute(
        "CREATE TABLE IF NOT EXISTS users (
            id INTEGER PRIMARY KEY,
            name VARCHAR,
            balance DECIMAL(10, 2)
        )",
    )?;

    // パラメータ付き INSERT (params! マクロ)
    conn.execute_params(
        "INSERT INTO users VALUES (?, ?, ?)",
        &params![1, "Alice", Decimal::from_str("100.50").unwrap()],
    )?;
    conn.execute_params(
        "INSERT INTO users VALUES (?, ?, ?)",
        &params![2, "Bob", Decimal::from_str("250.00").unwrap()],
    )?;

    // 型安全なクエリ走査 (Row::get_as)
    let rows = conn.query("SELECT id, name, balance FROM users WHERE balance > 150")?;
    for row in rows {
        let id: i32 = row.get_as(0)?;
        let name: String = row.get_as(1)?;
        let balance: Decimal = row.get_as(2)?;
        println!("User #{id}: {name} (${balance})");
    }

    Ok(())
}
```

### 2. 非同期 組み込みモード (Tokio)

```rust
use h2::{AsyncConnection, H2Result, params};

#[tokio::main]
async fn main() -> H2Result<()> {
    let conn = AsyncConnection::open("async_test.h2").await?;

    conn.execute("CREATE TABLE IF NOT EXISTS tasks (id INT PRIMARY KEY, title VARCHAR)").await?;
    conn.execute_params("INSERT INTO tasks VALUES (?, ?)", &params![1, "Hello Async"]).await?;

    let rows = conn.query("SELECT title FROM tasks WHERE id = 1").await?;
    if let Some(r) = rows.first() {
        let title: String = r.get_as(0)?;
        println!("Task: {title}");
    }

    Ok(())
}
```

---

## 📄 ライセンス

本プロジェクトは MPL-2.0 または Apache-2.0 のデュアルライセンスです。
H2 Database (Java) のライセンスおよび知的財産を尊重しています。