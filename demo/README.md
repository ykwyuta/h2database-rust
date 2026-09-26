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
| [**demo/006 同期レプリケーション**](./006_replication/README.md) | 高可用性 HA 構成 | `remote_apply`, Primary/Standby | Primary (RW) と Standby (RO) の2インスタンスを起動し、PostgreSQLの `remote_apply` 相当の同期レプリケーションと書き込み保護を実演するデモ |
| [**demo/007 トランザクショナルMQ**](./007_mq/README.md) | ネイティブメッセージキュー | `QUEUE TABLE`, JMS 2.0/3.0, Kafka-seeking | DB 同一トランザクションで動くキューテーブル、JMS API、Kafka 風オフセットシーク、二重保持ポリシーによる Head Truncation GC を実演するデモ |
| [**demo/008 Aurora型分離クラスタ**](./008_decoupled_aurora/README.md) | クラウドネイティブ分散ストレージ | 4/6 Quorum, 3 AZ, "The Log is the Database" | 3 AZ 6ノードの分散スマートストレージ、WAL ログのみ転送、ゼロストレージ・リードレプリカ、AZ 障害耐性、瞬間フェイルオーバーを実演するデモ |
| [**demo/009 Spring Boot & MyBatis**](./009_spring_boot_mybatis/README.md) | エンタープライズ Web / ORM | Spring Boot 4.1, MyBatis (XML), HikariCP | 全高度SQL（SEQUENCE, SERIAL, INTERVAL, 関数, カーソル）＋同期レプリケーションPrimary/Standby動的ルーティングを実演するデモ |
| [**demo/010 Spring Boot & Spring JMS**](./010_spring_boot_jms/README.md) | トランザクショナルMQ / イベント連携 | Spring Boot 4.1, Spring JMS, MyBatis | Spring 標準 JMS（`JmsTemplate`, `@JmsListener`）と MyBatis の同一トランザクション連携、Outbox パターン不要のアトミックコミット・ロールバック、Kafka 風シーク再生を実演するデモ |
| [**demo/011 専用CLI: 高度SQL**](./011_cli_advanced_sql/README.md) | 専用CLI・高度SQL | `h2-cli`, Online DDL, 再帰CTE, ウィンドウ関数 | 専用 CLI から実行する Instant/Online DDL, UPSERT, 再帰CTE, 6種のJOIN, 集合演算, ウィンドウ関数, 日本語全文検索, 外部キーCASCADE デモ |
| [**demo/012 専用CLI: システム・運用**](./012_cli_system_and_maintenance/README.md) | 専用CLI・運用保守 | `h2-cli`, SEQUENCE, カーソル, BACKUP/COPY | 専用 CLI から実行する シーケンス生成器, SERIAL, INTERVAL日時計算, サーバサイドカーソル走査, 高度数学・正規表現関数, CSVデータ移行, 物理バックアップ・リストア, VACUUM デモ |
| [**demo/013 専用CLI: トランザクショナルMQ**](./013_cli_transactional_mq/README.md) | 専用CLI・ネイティブMQ | `h2-cli`, `QUEUE TABLE`, アトミックロールバック | 専用 CLI から直接実行する `CREATE QUEUE TABLE`、アトミックコミット＆ロールバック（Outbox不要の実証）、Kafka風オフセットシーク再生、安全ガード実演デモ |
| [**demo/014 専用CLI: ユーザー認証・アクセス元制限・テーブル権限管理**](./014_cli_auth_and_permissions/README.md) | 専用CLI・セキュリティ | `h2-cli`, DCL, 認証, ホスト制限, 権限管理 | 専用 CLI から実行する ユーザー作成（CREATE USER）、パスワード認証、ホストIP/CIDR制限、テーブル単位の権限付与・剥奪（GRANT/REVOKE SELECT/INSERT/UPDATE/DELETE）、権限昇格拒否デモ |
| [**demo/015 Spring Boot & JMS: JmsTemplate & @JmsListener**](./015_spring_boot_jms_template_and_listener/README.md) | Spring JMS パターン集 | Spring Boot 3.4, Spring JMS, JmsTemplate, @JmsListener, @SendTo | Spring JMS の標準機能（`JmsTemplate.convertAndSend`、`receiveAndConvert`、`@JmsListener` 非同期購読、`@SendTo` による自動返信 RPC、`@Header` プロパティ注入、`@Transactional` アトミックロールバック）を網羅したデモ |
| [**demo/016 Spring Boot グラフDB デュアルIF**](./016_spring_boot_graph_dual_interface/README.md) | グラフDB (Bolt & SQL/JDBC) | Spring Boot 3.4, `neo4j-java-driver`, `JdbcTemplate` | 公式 Neo4j Java Driver (Bolt 7687) 経由の Cypher 操作と、PostgreSQL JDBC (PGWire 5432) 経由の仮想グラフテーブル・テーブル値関数（CYPHER TVF）によるハイブリッド JOIN を実演するデモ |

---

## 🏃 クイック実行コマンド一覧

```bash
# 1. 専用CLIの起動
cargo run -p h2-cli -- demo.h2

# 2. PostgreSQL 互換サーバーの起動 (psql接続用)
cargo run -p demo-psql-server

# 3. Rust 組み込み同期デモの実行 (高度なSQL関数、SEQUENCE、カーソル含む)
cargo run -p demo-embedded-sync

# 4. Rust 組み込み非同期デモの実行 (Tokio)
cargo run -p demo-embedded-async

# 5. Java JDBC デモの実行 (サーバー起動後)
cd demo/005_jdbc && mvn compile exec:java

# 6. 同期レプリケーション (remote_apply) デモの実行
cargo run -p demo-replication

# 7. トランザクショナル・キューテーブル & JMS API (Native MQ) デモの実行
cargo run -p demo-mq

# 8. AWS Aurora 型 コンピュート・ストレージ完全分離クラスタ デモの実行
cargo run -p demo-decoupled-aurora

# 9. Spring Boot 4.1 + MyBatis (XML) + レプリケーション動的ルーティング デモの実行
# (別ターミナルで cargo run -p demo-spring-boot-server を起動後)
cd demo/009_spring_boot_mybatis && run_demo.bat  # または mvn spring-boot:run

# 10. Spring Boot 4.1 + Spring JMS + MyBatis トランザクショナルMQ デモの実行
# (別ターミナルで cargo run -p demo-spring-boot-server を起動後)
cd demo/010_spring_boot_jms && run_demo.bat  # または mvn spring-boot:run

# 11. 専用CLI デモ 1: 高度な SQL & クエリ演算 デモの実行
cargo run -p h2-cli -- -f demo/011_cli_advanced_sql/script.sql

# 12. 専用CLI デモ 2: システム・拡張型・運用・カーソル デモの実行
cargo run -p h2-cli -- -f demo/012_cli_system_and_maintenance/script.sql

# 13. 専用CLI デモ 3: トランザクショナル・キューテーブル デモの実行
cargo run -p h2-cli -- -f demo/013_cli_transactional_mq/script.sql

# 14. 専用CLI デモ 4: ユーザー認証・アクセス元制限・テーブル権限管理 デモの実行
cargo run -p h2-cli -- -f demo/014_cli_auth_and_permissions/script.sql

# 15. Spring Boot & JMS (JmsTemplate & @JmsListener パターン集) デモの実行
# (別ターミナルで cargo run -p demo-spring-boot-server を起動後)
cd demo/015_spring_boot_jms_template_and_listener && run_demo.bat  # または mvn spring-boot:run

# 16. Spring Boot グラフDB デュアルインターフェース (Bolt & SQL/JDBC) デモの実行
# (別ターミナルで cargo run -p demo-graph-server を起動後)
cd demo/016_spring_boot_graph_dual_interface && run_demo.bat  # または mvn spring-boot:run
```
