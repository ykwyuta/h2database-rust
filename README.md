# h2database-rust (h2-rust)

[![Rust](https://img.shields.io/badge/rust-1.80%2B-orange.svg)](https://www.rust-lang.org)
[![License](https://img.shields.io/badge/license-MPL--2.0%20OR%20Apache--2.0-blue.svg)](LICENSE)

**SQLiteのように手軽に組み込めるが、SQLiteよりも圧倒的に高機能かつ並行性に優れた次世代組み込み／サーバーハイブリッド RDBMS ＆ グラフデータベース**

Java版 [H2 Database](https://github.com/h2database/h2database) の先進的なログ構造化 CoW (Copy-on-Write) B-Tree ストレージエンジン（**MVStore**）および PostgreSQL 互換ワイヤプロトコルの思想を受け継ぎ、Neo4j 互換グラフエンジン（**openCypher / Bolt**）、Apache Arrow ベクトル化エンジン、Aurora 型コンピュート・ストレージ分離アーキテクチャ、トランザクショナルネイティブ MQ を統合して Rust ネイティブでゼロから再構築されたマルチモデル・データベースです。

---

## 🌟 特長

- **🚀 組み込みファースト (Zero-Config, Single Library)**:
  - 外部プロセスの起動や設定ファイルは不要。`h2::Connection::open("mydb.h2")?` または `h2::Connection::open_in_memory()?` の1行で利用可能。
- **⚡ MVCC による高い並行書き込み性能**:
  - SQLiteのシングルライター制約（WAL時でも同時書き込みトランザクションは1つのみ）を打破。CoW B-Tree によるスナップショット分離（Snapshot Isolation）と追記型 WAL による超高速コミットを実現。
- **🕸️ Neo4j 互換グラフデータベースエンジン (`h2-graph` & `h2-bolt`)**:
  - **openCypher 準拠**: `CREATE`, `MATCH`, `MERGE`, `SET`, `DELETE`, `RETURN`, `ORDER BY`, `LIMIT` 構文を完備。
  - **高度なグラフ探索**: ラベル付きプロパティグラフ、インデックス支援ノード探索、可変長パス展開（`*1..5`）、最短経路探索（`shortestPath`）。
  - **Bolt プロトコル内蔵**: Neo4j 公式ドライバ（Java, Python, JS, Go 等）からポート `7687`（`bolt://`）経由で直接接続可能。
- **🔀 リレーショナル SQL ＆ グラフの完全統合 (Cypher-in-SQL / Virtual Graph Tables)**:
  - **仮想グラフテーブル**: グラフのノード・エッジを `SELECT * FROM graph_<name>_nodes`, `SELECT * FROM graph_<name>_edges` としてリレーショナル表形式で直接 SQL 照会。
  - **テーブル値関数 (TVF)**: `SELECT * FROM departments d JOIN CYPHER('company', 'MATCH (e:Employee) RETURN ...') AS g` により、RDBMS テーブルとグラフ探索結果を同一トランザクション・単一 SQL 文でハイブリッド結合。
- **📊 高度なSQL実行エンジン & インデックス**:
  - `UPDATE` / `DELETE USING` / `UPDATE FROM`、再帰 CTE（階層構造・連番生成・サイクル検出）、ウィンドウ関数（`ROW_NUMBER`, `RANK`, `DENSE_RANK`）、集合演算（`UNION`, `INTERSECT`, `EXCEPT`）、Instant / Online DDL、一意性・外部キーCASCADE制約、シーケンス（`SEQUENCE`, `SERIAL`, `IDENTITY`）、サーバサイドカーソル。
- **🏎️ Apache Arrow ベクトル化 ＆ 行ベースハイブリッド実行 (HTAP)**:
  - Slotted Page からの直接列転置（Direct Column Transposition）によるゼロコピー Arrow 変換、VectorChunk による SIMD 高速バッチ集約・フィルタリング。
- **📬 トランザクショナル・キューテーブル (Transactional Queue Table & Native MQ)**:
  - `CREATE QUEUE TABLE` によるデータベーストランザクション一体型メッセージキュー。JMS 2.0/3.0 準拠インターフェース、Kafka 風オフセットシーク（`seek`, `rewind`）、二重保持ポリシー（時間・容量上限）による自動 Head Truncation GC を完備し、Transactional Outbox パターンを完全不要化。
- **☁️ AWS Aurora 型 コンピュート・ストレージ完全分離アーキテクチャ (The Log is the Database)**:
  - コンピュートノードからストレージ層へは WAL ログレコードのみを転送（ダーティページ転送を完全撤廃）。
  - スマートストレージノードによる非同期 B-Tree マテリアライズとオンデマンド Redo 解決。
  - 4/6 Quorum（3 AZ）書き込みによる AZ 障害耐性と低速ディスクの遅延解消（Tail Latency Elimination）。
  - 同一ストレージ層を共有するゼロストレージ・リードレプリカ、および Fencing Token による瞬間フェイルオーバー。
- **🔍 日本語対応 N-Gram & 形態素解析 全文検索 (FTS)**:
  - 外部拡張不要で、バイグラム（2-gram）および文字種境界解析による形態素トークナイザーを内蔵。日本語テキストの高速全文検索をサポート。
- **🔌 内蔵 PostgreSQL 互換ワイヤプロトコル (PG-Wire)**:
  - アプリ内で組み込み動作させながら、オプションでポート `5432` を開放。稼働中のアプリを停止させずに **psql, DBeaver, DataGrip, VS Code 拡張, Spring Boot (JDBC/MyBatis)** から直接クエリを発行して管理・利用可能。
- **⚡ Tokio ネイティブ非同期 API (`AsyncConnection`)**:
  - `AsyncConnection::open(path).await` により Axum や Actix-web 等の非同期 Web サービスにそのまま統合可能。
- **🤖 Stateless Streamable-HTTP MCP (Model Context Protocol) Server**:
  - Claude Desktop や Cursor、自律型 AI エージェント向けに、ステートレスかつ高並行な DB / グラフアクセスプロトコルを提供。

---

## 📦 クレート構成

```text
h2database-rust/
├── crates/
│   ├── h2-types/       # 基本データ型 (DataType, Value)、エラー定義、矢印Arrowマッピング
│   ├── h2-mvstore/     # 追記型 CoW B-Tree ＆ MVCC ストレージエンジン (Slotted Page, Buffer Pool)
│   ├── h2-sql/         # SQLパーサ、カタログ、仮想グラフテーブル、オプティマイザ、実行エンジン
│   ├── h2-graph/       # Neo4j 互換グラフデータベースエンジン (openCypher 構文解析・トラバーサル実行)
│   ├── h2-bolt/        # Neo4j Bolt v4.4/v5.x バイナリプロトコルサーバー (PackStream)
│   ├── h2-server/      # PostgreSQL v3 プロトコルサーバー (PGWire)
│   ├── h2-cli/         # 対話型シェル REPL
│   └── h2/             # 最上位ファサードクレート (組み込みAPI, 非同期API, 統合サーバー起動)
├── demo/               # 16種類の実践デモプロジェクト (CLI, psql, Java JDBC, Spring Boot, MQ, Graph)
└── docs/               # 技術設計書・仕様ドキュメント (計22本)
```

---

## 📂 実践デモ集 (Demos)

本リポジトリには、主要な利用シナリオごとに独立して実行・検証できる **16種類のデモプロジェクト** を収録しています（詳細は [demo/README.md](./demo/README.md) を参照）。

| ディレクトリ | 対象 / ユースケース | 主な技術スタック |
| :--- | :--- | :--- |
| [**demo/001**](./demo/001_cli/README.md) | 専用対話型シェル | `h2-cli`, REPL, SQLスクリプト |
| [**demo/002**](./demo/002_psql/README.md) | PostgreSQL ツール接続 | PG-Wire, `psql`, DBeaver |
| [**demo/003**](./demo/003_embedded_sync/README.md) | Rust 組み込み同期 API | `Connection`, `params!`, `Row::get_as` |
| [**demo/004**](./demo/004_embedded_async/README.md) | Rust 組み込み非同期 API | `AsyncConnection`, Tokio, 並行タスク |
| [**demo/005**](./demo/005_jdbc/README.md) | Java JDBC クライアント | Java 17+, `org.postgresql.Driver` |
| [**demo/006**](./demo/006_replication/README.md) | 同期レプリケーション | `remote_apply`, Primary/Standby |
| [**demo/007**](./demo/007_mq/README.md) | ネイティブ MQ & キューテーブル | `QUEUE TABLE`, JMS 2.0/3.0, Kafka-seeking |
| [**demo/008**](./demo/008_decoupled_aurora/README.md) | Aurora 型分離クラスタ | 4/6 Quorum, 3 AZ, リードレプリカ |
| [**demo/009**](./demo/009_spring_boot_mybatis/README.md) | Spring Boot & MyBatis | Spring Boot, MyBatis (XML), HikariCP |
| [**demo/010**](./demo/010_spring_boot_jms/README.md) | Spring Boot & Spring JMS | Spring Boot, Spring JMS, MyBatis, Outbox不要 |
| [**demo/011**](./demo/011_cli_advanced_sql/README.md) | 専用CLI: 高度SQL | Instant DDL, 再帰CTE, ウィンドウ関数, FTS |
| [**demo/012**](./demo/012_cli_system_and_maintenance/README.md) | 専用CLI: システム・運用 | SEQUENCE, SERIAL, INTERVAL, カーソル, VACUUM |
| [**demo/013**](./demo/013_cli_transactional_mq/README.md) | 専用CLI: トランザクショナルMQ | キューテーブルDDL, アトミックロールバック実証 |
| [**demo/014**](./demo/014_cli_auth_and_permissions/README.md) | 専用CLI: 認証・権限管理 | CREATE USER, ホスト制限, GRANT/REVOKE |
| [**demo/015**](./demo/015_spring_boot_jms_template_and_listener/README.md) | Spring JMS パターン集 | `JmsTemplate`, `@JmsListener`, `@SendTo` |
| [**demo/016**](./demo/016_spring_boot_graph_dual_interface/README.md) | **Spring Boot グラフDB デュアルIF** | **Bolt (7687) + PostgreSQL JDBC (5432), Neo4j Java Driver, TVF JOIN** |

---

## 🛠️ クイックスタート

### 1. 同期 組み込みモード (Rust)

```rust
use h2::{Connection, H2Result, params};
use rust_decimal::Decimal;
use std::str::FromStr;

fn main() -> H2Result<()> {
    // データベースを開く（ファイル永続化またはインメモリ: open_in_memory()）
    let conn = Connection::open("mydb.h2")?;

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

    // 型安全なクエリ走査 (Row::get_as)
    let rows = conn.query("SELECT id, name, balance FROM users WHERE balance > 50")?;
    for row in rows {
        let id: i32 = row.get_as(0)?;
        let name: String = row.get_as(1)?;
        let balance: Decimal = row.get_as(2)?;
        println!("User #{id}: {name} (${balance})");
    }

    Ok(())
}
```

### 2. グラフDB ＆ リレーショナル SQL ハイブリッドクエリ

同一データベース内で、リレーショナルテーブルとグラフ探索（Cypher TVF）を SQL で直接 `JOIN` できます。

```rust
use h2::{Connection, H2Result};

fn main() -> H2Result<()> {
    let conn = Connection::open_in_memory()?;

    // 1. リレーショナルテーブルの作成
    conn.execute("CREATE TABLE departments (id INT PRIMARY KEY, name VARCHAR, location VARCHAR);")?;
    conn.execute("INSERT INTO departments VALUES (1, 'Engineering', 'Tokyo HQ'), (2, 'Sales', 'Osaka Branch');")?;

    // 2. グラフノードの作成 (Cypher TVF 経由)
    conn.execute("SELECT * FROM CYPHER('company', 'CREATE (:Employee {id: 101, name: ''Alice'', dept_id: 1})');")?;
    conn.execute("SELECT * FROM CYPHER('company', 'CREATE (:Employee {id: 102, name: ''Bob'', dept_id: 1})');")?;

    // 3. 仮想グラフテーブルの直接照会
    let node_rows = conn.query("SELECT id, labels, properties FROM graph_company_nodes;")?;
    println!("Graph Nodes: {:?}", node_rows);

    // 4. リレーショナルテーブルと CYPHER TVF のハイブリッド JOIN
    let rows = conn.query(
        "SELECT d.name AS dept, g.name AS emp
         FROM departments d
         JOIN CYPHER('company', 'MATCH (e:Employee) RETURN e.dept_id AS dept_id, e.name AS name')
              AS g(dept_id, name)
           ON d.id = CAST(g.dept_id AS INT)
         ORDER BY g.name;"
    )?;

    for row in rows {
        println!("Dept: {}, Employee: {}", row.get_as::<String>(0)?, row.get_as::<String>(1)?);
    }

    Ok(())
}
```

### 3. デュアルプロトコルサーバーの起動 (Bolt 7687 ＆ PGWire 5432)

単一のデータベースプロセスから、SQLクライアント（DBeaver, psql, JDBC）と Neo4j クライアント（Neo4j Desktop, neo4j-driver）の両方を受け付けます。

```rust
use h2::{Connection, H2Result};

#[tokio::main]
async fn main() -> H2Result<()> {
    let conn = Connection::open("shared.h2")?;

    // PostgreSQL Wire プロトコルサーバー起動 (ポート 5432)
    let pg_addr = conn.start_pg_server("127.0.0.1:5432".parse().unwrap()).await?;
    println!("SQL (PostgreSQL Wire) listening on: {pg_addr}");

    // Neo4j Bolt プロトコルサーバー起動 (ポート 7687, グラフ名: 'company')
    let bolt_addr = conn.start_bolt_server("127.0.0.1:7687".parse().unwrap(), "company").await?;
    println!("Graph (Neo4j Bolt) listening on: {bolt_addr}");

    // 接続待機
    tokio::signal::ctrl_c().await.unwrap();
    Ok(())
}
```

### 4. 非同期 組み込みモード (Tokio)

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

### 5. AWS Aurora 型 コンピュート・ストレージ分離クラスタ

```rust
use h2::storage::DecoupledCluster;
use h2::H2Result;

fn main() -> H2Result<()> {
    // 6ノード分散ストレージフリート (4 of 6 Quorum, 3 AZ) ＋ 2台のリードレプリカで起動
    let mut cluster = DecoupledCluster::new_6nodes("aurora-prod", 2)?;

    // 1. Primary でテーブル作成とデータ登録 (WAL ログレコードのみを並行クォーラム送信)
    let primary_conn = cluster.primary_connection();
    primary_conn.execute("CREATE TABLE accounts (id INT PRIMARY KEY, name VARCHAR, balance INT);")?;
    primary_conn.execute("INSERT INTO accounts VALUES (1, 'Alice', 1000);")?;

    // 2. ゼロストレージ・リードレプリカから即座に参照 (共有ストレージからオンデマンド読み出し)
    let replica_conn = cluster.replica_connection(0)?;
    let rows = replica_conn.query("SELECT * FROM accounts WHERE id = 1;")?;
    println!("Replica read: {:?}", rows[0].get_as::<String>(1)?);

    // 3. 瞬間フェイルオーバー (Fencing Token による新 Primary 昇格)
    let new_fencing_token = cluster.failover_to_replica(0)?;
    println!("Promoted to new primary with Fencing Token: {}", new_fencing_token.val());

    Ok(())
}
```

---

## 📚 ドキュメント

- 📖 **[利用者向け公式ガイド (User's Guide)](./docs/USER_GUIDE.md)**:
  PostgreSQL 公式ドキュメント構成をベースにした網羅的ガイド。データ型、SQL構文、トランザクション、日本語全文検索、非同期API、DBeaver接続手順までを解説。
- 🛠️ **[開発者・メンテナー向けガイド (Developer Guide)](./docs/DEVELOPER_GUIDE.md)**:
  プロジェクトの保守・拡張を行う開発者向けガイド。内部アーキテクチャ、CoW B-Tree、MVCC・UndoLog、新規型・構文の追加手順、ロック階層とデッドロック防止ルール、テスト方針を解説。
- ⚖️ **[SQL 標準規格 適合状況と機能比較 (SQL_STANDARDS_COMPLIANCE.md)](./docs/SQL_STANDARDS_COMPLIANCE.md)**:
  ISO/IEC 9075 SQL標準規格（SQL-92, SQL:1999, SQL:2003, SQL:2016 等）と対比した適合性マトリクス。

### 🏛️ アーキテクチャ設計・仕様ドキュメント（全22本）

1. [01. プロジェクトビジョンと技術比較](./docs/01_overview_and_vision.md)
2. [02. 全体アーキテクチャ設計](./docs/02_architecture_overview.md)
3. [03. ストレージエンジン設計（Rust版 MVStore）](./docs/03_storage_engine_mvstore.md)
4. [04. SQL処理系・型システム・実行エンジン](./docs/04_sql_parser_and_execution.md)
5. [05. 組み込みAPI・インターフェース設計](./docs/05_embedded_api_and_pgwire.md)
6. [06. 実装ロードマップとマイルストーン](./docs/06_roadmap_and_phases.md)
7. [07. トランザクショナル・キューテーブル設計](./docs/TRANSACTIONAL_QUEUE_TABLE_DESIGN.md)
8. [08. コンピュート・ストレージ分離アーキテクチャ設計 (Aurora Model)](./docs/08_decoupled_storage_architecture.md)
9. [09. メモリ管理機構の実装解説と他 RDBMS (PostgreSQL / SQL Server) との比較](./docs/09_memory_management_architecture_and_comparison.md)
10. [10. 統計情報収集・更新機構の調査報告](./docs/10_statistics_and_query_optimizer.md)
11. [11. Apache Arrow 互換ベクトル化実行と行ベース実行の両立方式設計書](./docs/11_arrow_vectorized_and_row_hybrid_execution.md)
12. [12. pgbench 性能評価レポート](./docs/12_pgbench_performance_evaluation.md)
13. [13. 性能最適化提案と実施結果](./docs/13_performance_optimization_proposal.md)
14. [14. UPDATE 性能の追加改善案](./docs/14_update_performance_additional_proposals.md)
15. [15. クエリ性能の計測と実行計画の確認](./docs/15_query_performance_metrics.md)
16. [16. UPDATE 性能の再測定と原因評価](./docs/16_update_performance_remeasurement.md)
17. [17. 再測定に基づく UPDATE 性能改善案](./docs/17_update_performance_improvement_plan.md)
18. [18. UPDATE 性能改善の実装結果](./docs/18_update_performance_implementation.md)
19. [19. PostgreSQL 18 対比 UPDATE 性能改善・検証シナリオ拡張レポート](./docs/19_update_performance_pg18_comparison.md)
20. [20. Stateless Streamable-HTTP MCP Server 設計方針](./docs/20_stateless_streamable_http_mcp_server_design.md)
21. [21. Neo4j 互換グラフデータベース（OpenCypher / Bolt）エンジン設計方針](./docs/21_neo4j_compatible_graph_engine_design.md)
22. [22. グラフDB × リレーショナル SQL 統合アクセス機能設計方針 (Cypher-in-SQL / Virtual Graph Tables)](./docs/22_sql_graph_integration_design.md)

---

## 📄 ライセンス

本プロジェクトは MPL-2.0 または Apache-2.0 のデュアルライセンスです。
H2 Database (Java) のライセンスおよび知的財産を尊重しています。