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
- **📊 高度なSQL実行エンジン**:
  - `UPDATE`（複数列更新・自己参照式計算）、`ORDER BY` / `LIMIT` / `OFFSET`（昇順降順・エイリアス対応）、集約関数（`COUNT`, `SUM`, `AVG`, `MIN`, `MAX`）＆ `GROUP BY` / `HAVING`、テーブル結合（`INNER JOIN` / `LEFT OUTER JOIN`）を標準搭載。
- **🛡️ 厳格でモダンな型システム**:
  - 動的型付け（Type Affinity）による不整合を防止。任意精度 `Decimal`、タイムゾーン付き `Timestamp`、ネイティブ `UUID`、`JSON` を完備。
- **🔍 日本語対応 N-Gram & 形態素解析 全文検索 (FTS)**:
  - 外部拡張不要で、N-Gram（バイグラム）および文字種境界解析による形態素トークナイザーを内蔵。日本語テキストのインデックス作成と高速な全文検索をサポート。
- **🔌 内蔵 PostgreSQL 互換ワイヤプロトコル (PG-Wire)**:
  - アプリ内で組み込み動作させながら、オプションでポート5432を開放。稼働中のアプリを停止させずに **DBeaver, DataGrip, VS Code拡張, psql** から直接クエリを発行してデバッグ・管理可能。


---

## 📚 ドキュメント

詳細なアーキテクチャ設計およびロードマップは [`docs/`](./docs/README.md) をご覧ください。

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

```rust
use h2::{Connection, Result};

fn main() -> Result<()> {
    // データベースを開く（またはインメモリ: open_in_memory()）
    let conn = Connection::open("test.h2")?;

    // テーブルの作成
    conn.execute(
        "CREATE TABLE users (
            id INTEGER PRIMARY KEY,
            name VARCHAR,
            balance DECIMAL(10, 2)
        )",
    )?;

    // データの挿入
    conn.execute("INSERT INTO users VALUES (1, 'Alice', 100.50)")?;
    conn.execute("INSERT INTO users VALUES (2, 'Bob', 250.00)")?;

    // クエリの実行
    let rows = conn.query("SELECT id, name, balance FROM users WHERE balance > 150")?;
    for row in rows {
        println!("User: {:?}, Balance: {:?}", row.get(1), row.get(2));
    }

    Ok(())
}
```

---

## 📄 ライセンス

本プロジェクトは MPL-2.0 または Apache-2.0 のデュアルライセンスです。
H2 Database (Java) のライセンスおよび知的財産を尊重しています。