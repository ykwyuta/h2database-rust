# H2 Database in Rust - 開発者・メンテナー向けガイド (Developer Guide)

本ドキュメントは、`h2database-rust` の保守・機能拡張・デバッグ・リファクタリングを担当する開発者およびメンテナーのための包括的な技術ガイドです。
内部アーキテクチャ、データ構造、ロック設計、新規機能追加の手順、テスト方針について解説します。

---

## 📚 目次

- [1. 全体アーキテクチャとクレート依存関係](#1-全体アーキテクチャとクレート依存関係)
- [2. ストレージエンジン (`h2-mvstore`) の内部仕様](#2-ストレージエンジン-h2-mvstore-の内部仕様)
  - [2.1 CoW (Copy-on-Write) B-Tree の構造](#21-cow-copy-on-write-b-tree-の構造)
  - [2.2 チャンク・ファイルフォーマットと CRC32 検証](#22-チャンクファイルフォーマットと-crc32-検証)
  - [2.3 MVCC トランザクション & スナップショット分離](#23-mvcc-トランザクション--スナップショット分離)
  - [2.4 コンパクション (Vacuum) の仕組み](#24-コンパクション-vacuum-の仕組み)
- [3. SQL 処理系 (`h2-sql`) の内部仕様](#3-sql-処理系-h2-sql-の内部仕様)
  - [3.1 クエリ実行パイプライン](#31-クエリ実行パイプライン)
  - [3.2 カタログとマップ命名規則](#32-カタログとマップ命名規則)
  - [3.3 セカンダリインデックス & IndexScan 最適化](#33-セカンダリインデックス--indexscan-最適化)
  - [3.4 全文検索エンジン (FTS)](#34-全文検索エンジン-fts)
- [4. サーバー層 (`h2-server`) の内部仕様](#4-サーバー層-h2-server-の内部仕様)
- [5. 最上位ファサード (`h2`) と API 設計](#5-最上位ファサード-h2-と-api-設計)
- [6. 新機能追加の手順 (How-to Guides)](#6-新機能追加の手順-how-to-guides)
  - [6.1 新しい SQL データ型を追加する](#61-新しい-sql-データ型を追加する)
  - [6.2 新しい SQL 構文・文を追加する](#62-新しい-sql-構文文を追加する)
  - [6.3 新しい SQL 関数・演算子を追加する](#63-新しい-sql-関数演算子を追加する)
- [7. 並行性・ロック設計とデッドロック防止ルール](#7-並行性ロック設計とデッドロック防止ルール)
- [8. テスト戦略と品質保証](#8-テスト戦略と品質保証)
- [9. 開発コマンド・チートシート](#9-開発コマンドチートシート)

---

## 1. 全体アーキテクチャとクレート依存関係

本プロジェクトは **Cargo ワークスペース** による明確なレイヤードアーキテクチャを採用しています。

```
                  ┌─────────────────────────────────────┐
                  │          demo/* (各種デモ)          │
                  └──────────────────┬──────────────────┘
                                     │
           ┌─────────────────────────┼─────────────────────────┐
           │                         ▼                         │
┌──────────┴──────────┐   ┌─────────────────────┐   ┌──────────┴──────────┐
│  h2-cli (対話シェル) │   │ h2 (最上位ファサード)│   │  外部アプリケーション │
└──────────┬──────────┘   └──────────┬──────────┘   └─────────────────────┘
           │                         │
           │         ┌───────────────┴───────────────┐
           │         ▼                               ▼
           │  ┌──────────────┐                ┌──────────────┐
           └─►│  h2-server   │                │    h2-sql    │
              │  (PG-Wire)   │───(クエリ実行)─►│(SQL実行エンジン│
              └──────────────┘                └──────┬───────┘
                                                     │
                                                     ▼
                                              ┌──────────────┐
                                              │  h2-mvstore  │
                                              │(追記CoW BTree│
                                              └──────┬───────┘
                                                     │
                                                     ▼
                                              ┌──────────────┐
                                              │   h2-types   │
                                              │(型・エラー定義)│
                                              └──────────────┘
```

### 各クレートの責務とルール

| クレート名 | パス | 責務 | 依存ルール |
| :--- | :--- | :--- | :--- |
| **`h2-types`** | `crates/h2-types/` | 基本データ型（`DataType`, `Value`）、`FromSql` トレイト、エラー型（`H2Error`, `H2Result`）。 | **他内部クレートへの依存は禁止**（最下層）。純粋なデータ型定義のみ。 |
| **`h2-mvstore`** | `crates/h2-mvstore/` | ログ構造化 CoW B-Tree、ファイル I/O、MVCC トランザクション、Undo Log、Vacuum コンパクション。 | `h2-types` のみに依存。**SQL の概念（テーブルやカラム）は一切持たない**（純粋なキーバリューストア）。 |
| **`h2-sql`** | `crates/h2-sql/` | SQL パーサ、カタログ（`_catalog`）、オプティマイザ、クエリ実行器、JOIN/GROUP BY/集約、全文検索（FTS）。 | `h2-types`, `h2-mvstore`, `sqlparser` に依存。ネットワークの概念は持たない。 |
| **`h2-server`** | `crates/h2-server/` | PostgreSQL v3 ワイヤプロトコルハンドラ、Tokio ネットワークサーバー。 | `h2-types`, `h2-sql` に依存。 |
| **`h2-cli`** | `crates/h2-cli/` | スタンドアロンの対話型 REPL 実行バイナリ。 | `h2` ファサードに依存。 |
| **`h2`** | `crates/h2/` | 利用者向けトップレベルファサード。同期 `Connection`、`params!` マクロ、非同期 `AsyncConnection`、PG サーバー起動。 | 全クレートを統合して再公開。 |

---

## 2. ストレージエンジン (`h2-mvstore`) の内部仕様

Java 版 H2 Database のコアである **MVStore** のアーキテクチャを Rust でゼロから実装した追記型ストレージエンジンです。

### 2.1 CoW (Copy-on-Write) B-Tree の構造

- **ファイル**: [`crates/h2-mvstore/src/tree.rs`](file:///d:/workspace/h2database-rust/crates/h2-mvstore/src/tree.rs), [`page.rs`](file:///d:/workspace/h2database-rust/crates/h2-mvstore/src/page.rs)
- **イミュータビリティ**: ページノード（`Arc<Page>`）は一度生成されると変更されません。更新・挿入・削除が発生した場合、ルートから対象リーフノードまでのパスのみを新しく複製・生成（Copy-on-Write）します。
- **ロックフリー読み取り**: 過去バージョンのルートページへの参照（`Arc<Page>`）を保持しているリーダーは、ライターの更新動作と一切競合せず、ロックフリーで過去スナップショットを探索できます。

### 2.2 チャンク・ファイルフォーマットと CRC32 検証

- **ファイル**: [`crates/h2-mvstore/src/file_store.rs`](file:///d:/workspace/h2database-rust/crates/h2-mvstore/src/file_store.rs)
- **ファイル先頭 (Header)**: マジックバイト `b"H2MV"` (4 bytes) + フォーマットバージョン (4 bytes)。
- **チャンク (Chunk)**:
  ```text
  [Chunk Length: 4B][CRC32: 4B][Serialized ChunkPayload (JSON / Bincode)]
  ```
- **コミット処理 (`commit`)**:
  各マップの最新ルートノードをルートツリー（メタデータツリー）に格納し、ペイロードをファイル末尾に追記。追記完了後に `sync_all()` でディスクにフラッシュします。
- **クラッシュリカバリ**: ファイル末尾から後方へ走査し、CRC32 チェックサムが正当な最後のチャンクを特定して安全に復元します（破損した途中書き込みチャンクは自動で破棄）。

### 2.3 MVCC トランザクション & スナップショット分離

- **ファイル**: [`crates/h2-mvstore/src/tx/`](file:///d:/workspace/h2database-rust/crates/h2-mvstore/src/tx/)
- **`VersionedValue`**:
  各キーの値は単一のバイト列ではなく、以下の構造で保存されます：
  - `active_tx_id`: 現在変更中のトランザクション ID（コミット前）。
  - `committed_history`: 過去に確定した `(version, value)` の履歴ベクタ。
- **書き込み競合 (Write Conflict)**:
  他トランザクションの `active_tx_id` が残っているキーを更新しようとすると、`H2Error::Transaction("Write conflict detected")` が発生します。
- **Undo Log & 自動ロールバック**:
  トランザクション内で行われた変更は `undo_log` に記録されます。`rollback()` 実行時、または `Transaction` 構造体がコミットされずに `Drop` された場合、Undo Log を逆順に再生して未確定値を破棄し、直前のコミット済み値に復元します。

### 2.4 コンパクション (Vacuum) の仕組み

- **ファイル**: [`crates/h2-mvstore/src/store.rs#L112`](file:///d:/workspace/h2database-rust/crates/h2-mvstore/src/store.rs#L112), [`file_store.rs#L95`](file:///d:/workspace/h2database-rust/crates/h2-mvstore/src/file_store.rs#L95)
- 追記型ストレージは更新を重ねるとファイルサイズが肥大化します。
- `compact()`（SQL の `VACUUM`）が呼ばれると：
  1. 現在アクティブな全マップの最新ルートページのみを抽出。
  2. 新しいチャンク `ChunkPayload` を構築。
  3. ファイルヘッダー直後（Offset 8）から新チャンクのみを書き直し、ファイルサイズを `set_len()` で物理的に切り詰めます（Truncate）。

---

## 3. SQL 処理系 (`h2-sql`) の内部仕様

### 3.1 クエリ実行パイプライン

```
SQL文字列 ──► parse_sql() ──► Statement ──► SQLEngine::execute_statement()
                                                    │
             ┌──────────────────────────────────────┴──────────────────────────────────────┐
             ▼                                      ▼                                      ▼
      DDL (CreateTable,                      DML (Insert, Update,                     DQL (Query)
       CreateIndex, Drop)                          Delete)                                 │
             │                                      │                                      ▼
             ▼                                      ▼                               execute_query()
     Catalog更新 + Commit                   Tx経由でマップ更新                              │
                                            + インデックス自動同期                          ├─ IndexScan / TableScan
                                                                                            ├─ JOIN (Nested Loop / Hash)
                                                                                            ├─ WHERE 評価
                                                                                            ├─ GROUP BY & 集約
                                                                                            ├─ HAVING 評価
                                                                                            └─ ORDER BY & LIMIT
```

### 3.2 カタログとマップ命名規則

テーブルやインデックスの定義は、システム専用マップ **`_catalog`** に JSON シリアライズされて永続化されます。

| オブジェクト | カタログ上のキー | ストレージ上の MVMap 名 | 格納される値 |
| :--- | :--- | :--- | :--- |
| **テーブルデータ** | `tbl:<table_name>` | `tbl_<table_name>` | キー: `row_id` (u64 LE)<br>値: `Row` の JSON バイト列 |
| **セカンダリインデックス** | `idx:<index_name>` | `idx_<table_name>_<index_name>` | キー: `Value` シリアライズ + `0x00` + `row_id` (BE)<br>値: 空ベクタ `vec![]` |
| **全文検索インデックス** | `fts:<table_name>_<col>` | `fts_<table_name>_<col>_<type>` | キー: トークン文字列<br>値: `HashSet<RowId>` のシリアライズ |

> [!WARNING]
> **大文字・小文字の正規化ルール**:
> SQL ではテーブル名やカラム名は大文字小文字を区別しない（Case-Insensitive）ため、カタログへの登録・検索時およびマップ名生成時は **必ず `.to_lowercase()` で正規化** してください。

### 3.3 セカンダリインデックス & IndexScan 最適化

- **ファイル**: [`crates/h2-sql/src/executor.rs#L480`](file:///d:/workspace/h2database-rust/crates/h2-sql/src/executor.rs#L480)
- `WHERE column = <literal>` のクエリが実行された際、対象カラムにセカンダリインデックスが存在する場合、オプティマイザが自動的に **IndexScan** を選択します。
- 全テーブルスキャンを行わず、`idx_...` マップから該当キーをプレフィックス走査して `row_id` を即座に特定し、O(log N) でレコードを取得します。
- **インデックス自動同期**:
  - `INSERT`: レコード保存と同時に各インデックスマップへキーを投入。一意インデックスの場合は重複を検知してエラー返却。
  - `UPDATE`: 変更前のカラム値に対するインデックスキーを `tx.remove` し、新値のキーを `tx.put`。
  - `DELETE`: 削除行のインデックスキーを `tx.remove`。

### 3.4 全文検索エンジン (FTS)

- **ファイル**: [`crates/h2-sql/src/fts/`](file:///d:/workspace/h2database-rust/crates/h2-sql/src/fts/)
- **`NGramTokenizer`**: 日本語・アルファベット・数字を 2-gram（文字単位バイグラム）に分解。
- **`MorphTokenizer`**: 漢字・ひらがな・カタカナ・アルファベットの文字種変化境界（Unicode Block境界）で分割し、意味のある単語トークンを抽出。
- **転置インデックス**: 各トークンをキーとし、出現行 ID の集合（`HashSet<u64>`）を B-Tree に保持。クエリ時は複数トークンの積集合（AND 演算）で高速絞り込み。

### 3.5 スキーマ変更 (ALTER TABLE) & TRUNCATE TABLE & 診断構文

- **ファイル**: [`crates/h2-sql/src/executor.rs`](file:///d:/workspace/h2database-rust/crates/h2-sql/src/executor.rs)
- **`ALTER TABLE ... RENAME TO`**:
  - `tbl_<old>` から `tbl_<new>` へレコードを移行し、関連する `idx_<old>_<idx>` も `idx_<new>_<idx>` へ移行して古いマップを `store.remove_map` で破棄。カタログのテーブル定義およびインデックス定義を更新します。
- **`ALTER TABLE ... ADD COLUMN`**:
  - カタログの `TableDef` に新カラムを追加。既存レコードの末尾に `Value::Null` を補完して保存します。
- **`ALTER TABLE ... DROP COLUMN`**:
  - カタログの `TableDef` からカラムを削除。既存レコードから該当位置の値を削除して保存します。
- **`TRUNCATE TABLE`**:
  - テーブルマップおよび関連インデックスマップ内のキーを全件削除し、カタログの `next_row_id` を 1 にリセットします。
- **`EXPLAIN`**:
  - クエリ AST を解析し、スキャン種別（`IndexScan` vs `TableScan`）、結合アルゴリズム（`NestedLoopJoin`）、Filter 条件、Aggregate、Sort、Limit/Offset を分かりやすいツリー構造で出力します。
- **`SHOW TABLES` / `SHOW COLUMNS`**:
  - カタログ情報を走査し、対話型 CLI や GUI ツールで扱いやすい結果セット形式（Table 列、Field/Type/Null/Key 列）で返却します。

---

## 4. サーバー層 (`h2-server`) の内部仕様

- **ファイル**: [`crates/h2-server/src/pg_server.rs`](file:///d:/workspace/h2database-rust/crates/h2-server/src/pg_server.rs)
- Tokio の `TcpListener` で非同期待機。クライアントが接続するとセッションタスクを `tokio::spawn` します。
- **プロトコルハンドシェイク**:
  1. SSL 要求 (`SSLRequest: 80877103`): 平文接続を促すため `'N'` を返却。
  2. スタートアップメッセージ (`StartupMessage: 196608`): ユーザー名と DB 名を取得。
  3. 認証要求 (`AuthenticationOk: 'R', 0`): パスワードなしで即座に承認。
  4. パラメータステータス通知 (`client_encoding=UTF8`, `server_version=15.0 (h2database-rust)` 等) を送信。
  5. 準備完了通知 (`ReadyForQuery: 'Z', 'I'`): アイドル状態でクエリ待機。
- **Simple Query (`'Q'`) の処理**:
  クエリ文字列を受信し、`SQLEngine::execute()` を呼び出し。結果に応じて `RowDescription` (`'T'`), `DataRow` (`'D'`), `CommandComplete` (`'C'`), `ReadyForQuery` (`'Z'`) を返却。エラー時は `ErrorResponse` (`'E'`) を送出。

---

## 5. 最上位ファサード (`h2`) と API 設計

- **ファイル**: [`crates/h2/src/lib.rs`](file:///d:/workspace/h2database-rust/crates/h2/src/lib.rs), [`async_conn.rs`](file:///d:/workspace/h2database-rust/crates/h2/src/async_conn.rs)
- **`Connection`**:
  - `store: Arc<MVStore>` と `engine: Arc<SQLEngine>` を保持。
  - `current_tx: Arc<Mutex<Option<Transaction>>>` を内蔵し、SQL 文字列としての `BEGIN`, `COMMIT`, `ROLLBACK` が発行された場合にセッション内トランザクションを自動追跡。
- **`Row::get_as::<T: FromSql>(idx)`**:
  - `FromSql` トレイトによって、SQL の `Value` から Rust のネイティブ型（`i32`, `String`, `Decimal`, `Uuid`, `Option<T>` など）へ安全にデシリアライズ。
- **`AsyncConnection` / `AsyncTransaction`**:
  - `tokio::task::spawn_blocking` を活用し、イベントループをブロッキングすることなく非同期実行。

---

## 6. 新機能追加の手順 (How-to Guides)

メンテナーがよく行う拡張作業のステップバイステップ手順です。

### 6.1 新しい SQL データ型を追加する

1. **`crates/h2-types/src/data_type.rs`**:
   - `DataType` 列挙型に新しいバリアントを追加。
2. **`crates/h2-types/src/value.rs`**:
   - `Value` 列挙型に内部表現バリアントを追加。
   - `PartialOrd`（順序比較）、`Display`（文字列表現）、`From<T>` を実装。
   - `FromSql` トレイトにその型へのデシリアライズ実装を追加。
3. **`crates/h2-sql/src/parser.rs`**:
   - `convert_data_type()` に `sqlparser::ast::DataType` からの変換を追加。
4. **`crates/h2/tests/`**:
   - 単体テスト・統合テストを追加して検証。

### 6.2 新しい SQL 構文・文を追加する

1. **`crates/h2-sql/src/executor.rs`**:
   - `execute_statement()` の `match stmt` に対象の `sqlparser::ast::Statement` バリアントを追加。
   - 必要に応じてカタログ操作や `tx.put` / `tx.remove` を実装。
2. **テストの追加**:
   - `crates/h2/tests/` または各クレート内のテストにテストケースを追加。

### 6.3 新しい SQL 関数・演算子を追加する

1. **`crates/h2-sql/src/expression.rs`**:
   - `evaluate_expr()` の関数呼び出し `Expr::Function(func)` または演算子 `Expr::BinaryOp` に処理を追加。
   - `RowContext` から引数を評価し、計算結果の `Value` を返却。
2. **集約関数の場合**:
   - `crates/h2-sql/src/executor.rs` の `execute_aggregation()` 内でアキュムレータを更新・確定するロジックを追加。

---

## 7. 並行性・ロック設計とデッドロック防止ルール

本データベースは高い並行性を誇りますが、複数リソースに対するロックの順序を誤るとデッドロックを引き起こします。以下の **ロック階層ルール（Lock Hierarchy）** を厳格に遵守してください。

### ロック獲得の優先順位 (高 ──► 低)
```text
1. TransactionStore::active_transactions (RwLock)
2. MVStore::maps (RwLock)
3. Catalog::tables / Catalog::indexes (RwLock)
4. MVMap::tree (RwLock)
5. FileStore (RwLock)
```

> [!CAUTION]
> **デッドロック防止のための絶対ルール**:
> - `MVMap::tree` のロックを保持したまま `MVStore::maps` の write ロックを取得してはならない。
> - `FileStore` の write ロックを保持した状態で別スレッドの同期を待ってはならない。
> - すべてのロックには `std::sync` ではなく **`parking_lot`**（`parking_lot::RwLock`, `parking_lot::Mutex`）を使用すること。

---

## 8. テスト戦略と品質保証

本プロジェクトでは、コードの堅牢性を担保するため多層的なテストスイートを備えています。

```bash
# 全ワークスペーステストの実行 (必須)
cargo test --workspace

# 特定の統合テストのみを実行
cargo test --test h2_comprehensive_tests
cargo test --test new_planned_features_tests
cargo test --test advanced_features_tests
cargo test --test advanced_sql_tests
```

### 主要な統合テストファイル

- **[`h2_comprehensive_tests.rs`](file:///d:/workspace/h2database-rust/crates/h2/tests/h2_comprehensive_tests.rs)**:
  Java版 H2 Database を参考にした包括的ストレステスト。
  - `test_h2_mvcc_stress_and_concurrency`: 多数の並行リーダーとライターのスナップショット分離検証。
  - `test_h2_compaction_vacuum_stress`: 大量削除後の Vacuum ファイル縮小とデータ整合性検証。
  - `test_h2_crash_recovery_integrity`: プロセスクラッシュ模倣後のデータ復元と CRC32 検証。
- **[`new_planned_features_tests.rs`](file:///d:/workspace/h2database-rust/crates/h2/tests/new_planned_features_tests.rs)**:
  `DROP TABLE/INDEX`、`FromSql` / `get_as`、明示的トランザクション、`AsyncConnection` の検証。
- **[`advanced_sql_tests.rs`](file:///d:/workspace/h2database-rust/crates/h2/tests/advanced_sql_tests.rs)**:
  `JOIN`、`GROUP BY`、集約関数、`ORDER BY`、`LIMIT / OFFSET`、`UPDATE` の動作検証。

---

## 9. 開発コマンド・チートシート

```bash
# 1. ワークスペース全体のコンパイルチェック
cargo check --workspace

# 2. 静的解析 (Clippy)
cargo clippy --workspace --all-targets -- -D warnings

# 3. コードフォーマットチェック
cargo fmt --all -- --check

# 4. 全テストの実行
cargo test --workspace

# 5. CLI シェルのデバッグ起動
cargo run -p h2-cli -- debug.h2

# 6. PG-Wire デモサーバーの起動
cargo run -p demo-psql-server
```

---

メンテナーとしてのコミットや機能改善を歓迎します。疑問点があれば Issue や Discussion でお気軽にご相談ください！
Happy hacking on **H2 Database in Rust**!
