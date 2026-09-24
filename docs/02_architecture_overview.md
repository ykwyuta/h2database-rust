# 02. 全体アーキテクチャ設計

## 1. レイヤードアーキテクチャ

システムは関心の分離とプラガブル性を最大化するため、明確な階層構造（レイヤードアーキテクチャ）として設計します。

```text
┌────────────────────────────────────────────────────────────────────────┐
│                        Interface & Access Layer                        │
│  ┌─────────────────────────┐  ┌─────────────────────────────────────┐  │
│  │ Embedded API (rusqlite風) │  │ PG Wire Protocol Server (Tokio)     │  │
│  │ - Sync: Blocking API    │  │ - psql, DBeaver, JDBC, async-pg etc.│  │
│  │ - Async: Tokio async API│  │ - Startup, Query, Extended Query    │  │
│  └─────────────────────────┘  └─────────────────────────────────────┘  │
└────────────────────────────────────┬───────────────────────────────────┘
                                     │
┌────────────────────────────────────▼───────────────────────────────────┐
│                      SQL Engine Layer (Query Engine)                    │
│  ┌───────────────────────┐  ┌───────────────────────────────────────┐  │
│  │ SQL Parser & Lexer    │  │ Catalog & Metadata Manager            │  │
│  │ (sqlparser-rs + Ext)  │  │ (Schemas, Tables, Indexes, Sequences) │  │
│  └──────────┬────────────┘  └───────────────────┬───────────────────┘  │
│             │ AST                               │                      │
│  ┌──────────▼────────────┐  ┌───────────────────▼───────────────────┐  │
│  │ Query Planner/Binder  ├──► Physical Plan Optimizer (Cost-based)  │  │
│  └───────────────────────┘  └───────────────────┬───────────────────┘  │
│                                                 │ Physical Plan        │
│  ┌──────────────────────────────────────────────▼───────────────────┐  │
│  │ Query Execution Engine (Vectorized / Volcano Iterator Model)     │  │
│  │ - SeqScan, IndexScan, NestedLoopJoin, HashJoin, Aggregate, Sort │  │
│  │ - Expression Evaluator (Rust Native Functions, JIT Ready)        │  │
│  └──────────────────────────────────────────────┬───────────────────┘  │
└────────────────────────────────────┬────────────┘                      │
                                     │ Read/Write Operations             │
┌────────────────────────────────────▼───────────────────────────────────┐
│                    Transaction & Concurrency Layer                     │
│  ┌──────────────────────────────────────────────────────────────────┐  │
│  │ MVCC Manager (Snapshot Isolation, Read Committed)                 │  │
│  │ - Transaction ID & Commit Timestamp Allocator                    │  │
│  │ - Active Transactions Set & Version Visibility Filter            │  │
│  │ - Optimistic Concurrency Control (OCC) / Conflict Detection      │  │
│  └──────────────────────────────────┬───────────────────────────────┘  │
└─────────────────────────────────────┼──────────────────────────────────┘
                                      │ Versioned KV Operations          │
┌─────────────────────────────────────▼──────────────────────────────────┐
│                   Storage Engine Layer (Rust MVStore)                  │
│  ┌────────────────────────┐  ┌──────────────────────────────────────┐  │
│  │ B-Tree Index / Map     │  │ Chunk & Page Manager                 │  │
│  │ (Append-Only CoW Tree) │  │ (Slotted Pages, Block Allocator)     │  │
│  └──────────┬─────────────┘  └──────────────────┬───────────────────┘  │
│             │                                   │                      │
│  ┌──────────▼─────────────┐  ┌──────────────────▼───────────────────┐  │
│  │ Buffer Pool / PageCache│  │ Background GC & Compactor            │  │
│  │ (Clock-Pro / 2Q Cache) │  │ (Chunk Rewriter, Hole Punching)      │  │
│  └──────────┬─────────────┘  └──────────────────┬───────────────────┘  │
│             │                                   │                      │
│  ┌──────────▼───────────────────────────────────▼───────────────────┐  │
│  │ File System Abstraction (VFS)                                    │  │
│  │ - Local Posix/Windows File (DirectIO/mmap)                        │  │
│  │ - In-Memory Virtual File (RamFS)                                 │  │
│  │ - Encryption Layer (AES-256-GCM / ChaCha20)                       │  │
│  └──────────────────────────────────────────────────────────────────┘  │
└────────────────────────────────────────────────────────────────────────┘
```

---

## 2. Cargo ワークスペースとクレート分割方針

単一巨大モノリスではなく、用途ごとに再利用可能な Cargo ワークスペース構成を採用します。

```text
h2database-rust/
├── Cargo.toml                  # Workspace 定義
├── crates/
│   ├── h2-types/               # 共通型定義 (Value, DataType, Schema, Error)
│   ├── h2-mvstore/             # 独立したストレージエンジン (CoW B-Tree, MVCC KV)
│   ├── h2-sql/                 # パーサ、プランナ、オプティマイザ、実行エンジン
│   ├── h2-server/              # PostgreSQL ワイヤプロトコルサーバー
│   ├── h2-cli/                 # REPL CLIツール (psql / sqlite3 互換CLI)
│   └── h2/                     # 最上位ファサードクレート (組み込みAPI & エントリポイント)
└── docs/                       # 設計・仕様ドキュメント
```

### 各クレートの責務

| クレート名 | 依存関係 | 責務 |
| :--- | :--- | :--- |
| `h2-types` | なし (serde, bytes 等) | 基本データ型（INTEGER, VARCHAR, DECIMAL, UUID, JSON, VECTOR 等）、Value列挙型、エラー型、シリアライゼーション規約。 |
| `h2-mvstore` | `h2-types` | H2のMVStoreに相当する汎用高機能追記型キーバリューストア。B-Tree、Chunk管理、CoW、MVCCトランザクション、WAL、コンパクション。SQLに非依存な純粋ストレージとして単体利用も可能。 |
| `h2-sql` | `h2-types`, `h2-mvstore` | SQLパーサ（sqlparser-rs拡張）、カタログ（DDL、テーブル、インデックス）、論理/物理プランナ、ルールベース/コストベース最適化、行/ベクトル化エグゼキュータ。 |
| `h2-server` | `h2-sql`, `h2-types` | TokioベースのPostgreSQL v3 ワイヤプロトコル実装。SSL/TLS暗号化、セッション管理、認証。 |
| `h2` | `h2-sql`, `h2-server`, `h2-mvstore` | エンドユーザーが `cargo add h2` で使うメインクレート。`Connection`, `Statement`, `Transaction` 等の直感的APIを提供し、オプション機能（pgwireサーバー起動等）を集約。 |
| `h2-cli` | `h2` | ターミナルから操作するスタンドアロン対話型シェル（CLI）。テーブル表示、履歴、自動補完。 |

---

## 3. スレッドモデルと並行性設計

### 組み込み同期モード (Sync In-Process)
- アプリケーション側のスレッドから直接 `Connection` を呼び出します。
- リーダー（SELECT等）は共有ロックまたは完全ロックフリー（スナップショットに基づくCoW参照）。
- ライター（INSERT/UPDATE/DELETE/DDL）は、MVCCマネージャを介して独立した作業バッファに変更を蓄積し、コミット時に追記型ログに書き出します。

### 非同期・サーバーモード (Async & Server)
- 内部コアロジックは非同期にブロッキングを起こさない設計、またはCPUバウンド処理をRayonやTokioの `spawn_blocking` で効率的に分散。
- I/O処理は `tokio::fs` または非同期VFS抽象レイヤーを通じて処理。

```text
クライアント 1 ──┐
クライアント 2 ──┼─► [Tokio Network Listener] ─► [Session 1] ─┐
組み込みスレッド ─┼─► [Direct In-Process API] ──► [Session 2] ─┼─► [MVCC Transaction Store] ─► [Disk CoW Pages]
DBeaver (PGWire) ┘                                             │
                                                               ▼
                                                       [Background Vacuum Task]
```
