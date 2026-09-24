# H2 Database Rust (h2-rust) ドキュメント

本ディレクトリは、Java製リレーショナルデータベース「H2 Database」を参考に、**「SQLiteのように手軽に組み込めるが、SQLiteよりも圧倒的に高機能かつ並行性に優れた次世代組み込み／サーバーハイブリッドRDBMS」**をRustで設計・実装するための技術仕様および実装ロードマップです。

## ドキュメント一覧

- 📖 **[利用者向け公式ガイド (USER_GUIDE.md)](./USER_GUIDE.md)**:
  PostgreSQL 公式ドキュメントの構成をベースに体系化した、データベース利用者向けの実践的総合マニュアル。データ型、SQL構文、トランザクション、日本語全文検索、非同期API、DBeaver接続手順などを網羅。

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
