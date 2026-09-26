# H2 Database Rust (h2-rust) ドキュメント

本ディレクトリは、Java製リレーショナルデータベース「H2 Database」を参考に、**「SQLiteのように手軽に組み込めるが、SQLiteよりも圧倒的に高機能かつ並行性に優れた次世代組み込み／サーバーハイブリッドRDBMS」**をRustで設計・実装するための技術仕様および実装ロードマップです。

## ドキュメント一覧

- 🔒 **[セキュリティテストケース](./testcases/security/README.md)**: 認証・権限、PGWire、MCP、永続化・資源制限の攻撃入力と合格基準。
- 🧰 **[運用性テストケース](./testcases/operation/README.md)**: バックアップ、監視、計画制御、自動最適化、VACUUM・断片化の手動検証。
- 📋 **[運用性レビュー](./review/operation/README.md)**: PostgreSQL・SQL Server・Oracle と現行実装の差分、優先順位。
- 📋 **[PostgreSQL 18 PL/pgSQL 互換性対応表](./review/plpgsql/README.md)**: 公式仕様と手続きシミュレーション層の対応範囲・差分。
- 🌐 **[PostgreSQL 18 ICU 照合順序の調査報告](./reports/postgresql_18_icu_collation_control.md)**: 指定方法、非決定的比較、性能・運用上の注意点。
- 🌐 **[SQL Server 照合順序の調査報告](./reports/sql_server_collation_control.md)**: 指定範囲、感度オプション、優先順位、一時表・移行時の注意点。
- 📖 **[利用者向け公式ガイド (USER_GUIDE.md)](./USER_GUIDE.md)**:
  PostgreSQL 公式ドキュメントの構成をベースに体系化した、データベース利用者向けの実践的総合マニュアル。データ型、SQL構文、トランザクション、日本語全文検索、非同期API、DBeaver接続手順などを網羅。
- 🛠️ **[開発者・メンテナー向けガイド (DEVELOPER_GUIDE.md)](./DEVELOPER_GUIDE.md)**:
  プロジェクトの保守・拡張を行う開発者向けガイド。内部アーキテクチャ、CoW B-Tree、MVCC・UndoLog、新規型・構文の追加手順、ロック階層とデッドロック防止ルール、テスト方針を解説。
- ⚖️ **[SQL 標準規格 適合状況と機能比較 (SQL_STANDARDS_COMPLIANCE.md)](./SQL_STANDARDS_COMPLIANCE.md)**:
  ISO/IEC 9075 SQL標準規格（SQL-92, SQL:1999, SQL:2003, SQL:2016 等）と対比し、何が実装できていて何が未実装・制限事項かを網羅した適合性マトリクス。

### アーキテクチャ・設計仕様書

1. [01. プロジェクトビジョンと技術比較 (01_overview_and_vision.md)](./01_overview_and_vision.md)
   - なぜ作るのか、誰のためのデータベースか
   - SQLite、Java版H2 Database、他のRust製DBとの徹底比較
   - コアバリュー（組み込みファースト、MVCC並行性、厳格な型システム、AI/ベクトル検索、Postgres接続性）

2. [02. 全体アーキテクチャ設計 (02_architecture_overview.md)](./02_architecture_overview.md)
   - レイヤードアーキテクチャ（Interface、Engine、Planner/Executor、Storage）
   - クレート（Workspace）分割方針
   - プロセスモデル、スレッド/非同期タスクモデル

3. [03. ストレージエンジン設計（Rust版 MVStore） (03_storage_engine_mvstore.md)](./03_storage_engine_mvstore.md)
   - H2のMVStore（Multi-Version Store）をベースとした設計
   - チャンク追記型 CoW (Copy-on-Write) B-Tree
   - MVCCトランザクション管理（Snapshot Isolation、OCC、Rollback）
   - コンパクション（GC/Vacuum）、クラッシュリカバリ、インメモリモード

4. [04. SQL処理系・型システム・実行エンジン (04_sql_parser_and_execution.md)](./04_sql_parser_and_execution.md)
   - パーサ選定と拡張（sqlparser-rs + H2/PG方言）
   - 厳格かつリッチな型システム（Decimal, DateTime, UUID, JSON, Vector）
   - カタログ（Schema, Table, Index, Sequence, View）
   - 論理・物理最適化およびPull型/ベクトル化実行モデル

5. [05. 組み込みAPI・インターフェース設計 (05_embedded_api_and_pgwire.md)](./05_embedded_api_and_pgwire.md)
   - SQLiteライクなエルゴノミックな同期組み込みAPI（rusqlite互換ライク）
   - Tokioベースのネイティブ非同期API
   - PostgreSQL v3 ワイヤプロトコルサーバー（GUI/CLI/既存エコシステムとのシームレス連携）

6. [06. 実装ロードマップとマイルストーン (06_roadmap_and_phases.md)](./06_roadmap_and_phases.md)
   - フェーズ1（コアストレージと基本KV）からフェーズ5（高機能拡張・製品品質）までの段階的開発計画
   - テスト戦略（sqllogictest、Jepsen風クラッシュテスト、ベンチマーク）

7. [07. トランザクショナル・キューテーブル設計 (TRANSACTIONAL_QUEUE_TABLE_DESIGN.md)](./TRANSACTIONAL_QUEUE_TABLE_DESIGN.md)
   - DB 同一トランザクション下で扱えるネイティブ MQ（Transactional Outbox の完全解消）
   - キューテーブル（Queue Table）としての SQL 透過性（`INSERT` / `SELECT` 対応、`UPDATE`/`DELETE`/追加インデックス禁止、`_offset` WHERE 句制限）
   - JMS 2.0/3.0 準拠インターフェースと Kafka 風オフセットシーク（`seek`, `rewind`, timestamp seek）
   - 二重保持ポリシー（保持期間 & 容量上限超過時のオンライン Head Truncation GC）

8. [08. コンピュート・ストレージ分離アーキテクチャ設計 (08_decoupled_storage_architecture.md)](./08_decoupled_storage_architecture.md)
   - AWS Aurora / AlloyDB 型のコンピュート・ストレージ完全分離設計
   - "The Log is the Database"（ネットワーク越しには WAL ログレコードのみ送信、ダーティページ転送の完全撤廃）
   - 共有分散ストレージフリートとゼロストレージ・リードレプリカ（追加ストレージコスト 0）
   - 4/6 クォーラム書き込み、AZ 障害耐性、ピアツーピア・ゴシップ自己修復
   - 組み込みモード（Local MVStore）と分離モード（Distributed LogStore）のハイブリッド統合

9. [09. メモリ管理機構の実装解説と他 RDBMS (PostgreSQL / SQL Server) との比較・改善提案 (09_memory_management_architecture_and_comparison.md)](./09_memory_management_architecture_and_comparison.md)
   - 現行実装（MVStore, Executor, MVCC）のメモリモデルとアロケーション挙動の詳細分析
   - PostgreSQL（MemoryContext, shared_buffers, work_mem）および SQL Server（SOS, Buffer Pool, Memory Grant, Columnstore）との徹底比較
   - 現状の 5 つの重大な課題（RAM上限、全ツリー書き出し、Heap Churn、メモリ肥大化、OOMリスク）
   - 4フェーズにわたる段階的改善ロードマップ（アリーナ・Slotted Row、Buffer Pool、外部ソート・Memory Grant、ベクトル化）

10. [10. 統計情報収集・更新機構の調査報告 (10_statistics_and_query_optimizer.md)](./10_statistics_and_query_optimizer.md)
    - 現行実装に統計情報収集・更新機構が**完全に存在しない**ことの確認（全キーワード検索 0 件）
    - カタログ構造（TableDef / IndexDef）における統計フィールドの欠落分析
    - EXPLAIN 実装の実態（コストなし・構文整形のみ）とインデックス選択のルールベース判定
    - PostgreSQL (`pg_statistic`, autovacuum ANALYZE) / SQL Server / Java 版 H2 との機能比較
    - 4 つの問題点（行数不明・カーディナリティ不明・JOIN 順序固定・Range Scan 未対応）
    - 段階的改善ロードマップ（近似行数カウンタ・ANALYZE 文・選択率推定・コストベース JOIN 最適化）

11. [11. Apache Arrow 互換ベクトル化実行と行ベース実行の両立方式設計書 (11_arrow_vectorized_and_row_hybrid_execution.md)](./11_arrow_vectorized_and_row_hybrid_execution.md)
    - なぜ Arrow ベクトル化と行ベース（Volcano）の両立（HTAP）が必要か
    - モルフォロジック（多態的）ハイブリッド実行アーキテクチャ
    - VectorChunk 設計と `h2_types::Value` ⇔ Apache Arrow 型マッピング
    - デュアル Operator Trait と境界アダプタ（RowToVector / VectorToRow）
    - Slotted Page からの直接転置（Direct Column Transposition）によるゼロコピー化
    - 統計情報基盤と CBO を連携した自動実行パス判定（Adaptive Rule & Runtime Promotion）
    - DuckDB, SQL Server (Batch Mode on Rowstore) 等とのアーキテクチャ比較と段階的ロードマップ

### 性能評価・改善提案

- [12. pgbench 性能評価](./12_pgbench_performance_evaluation.md)
- [13. 性能最適化提案と実施結果](./13_performance_optimization_proposal.md)
- [14. UPDATE 性能の追加改善案](./14_update_performance_additional_proposals.md)
- [15. クエリ性能の計測と実行計画の確認](./15_query_performance_metrics.md)
- [16. UPDATE 性能の再測定と原因評価](./16_update_performance_remeasurement.md)
- [17. 再測定に基づく UPDATE 性能改善案](./17_update_performance_improvement_plan.md)
- [18. UPDATE 性能改善の実装結果](./18_update_performance_implementation.md)
- [19. PostgreSQL 18 対比 UPDATE 性能改善・検証シナリオ拡張レポート](./19_update_performance_pg18_comparison.md)
- [20. Stateless Streamable-HTTP MCP Server 設計方針](./20_stateless_streamable_http_mcp_server_design.md)
- [21. Neo4j 互換グラフデータベース（OpenCypher / Bolt）エンジン設計方針](./21_neo4j_compatible_graph_engine_design.md)
- [22. グラフDB × リレーショナル SQL 統合アクセス機能設計方針 (Cypher-in-SQL / Virtual Graph Tables / SQL:2023 PGQ)](./22_sql_graph_integration_design.md)
- [23. PL/pgSQL 手続きシミュレーション層の設計方針](./23_plpgsql_procedure_simulation_design.md)
- [24. 高速バイナリバックアップ・PITR・リードレプリカバックアップ設計方針](./24_high_performance_binary_backup_pitr_and_replica_backup_design.md)
