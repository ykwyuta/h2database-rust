# 28. ソース規模適正化・後方互換コード削除・モジュール分割リファクタリング方針書

**作成日**: 2026-09-26  
**ステータス**: 設計完了・実装中  
**対象クレート**: `h2-sql` (および関連クレート)

---

## 1. 背景と課題

`h2database-rust` は機能の継続的拡張（高度な SQL 構文、ベクトル化実行、トランザクショナル・キューテーブル、Cypher グラフ統合、多言語方言モード、Auto-Vacuum/Auto-Analyze 等）を経て、極めて高い機能網羅性を獲得しました。

しかしながら、以下の設計的課題が顕在化しています：

1. **単一ファイルの肥大化（モノリス化）**:
   - [`crates/h2-sql/src/executor.rs`](file:///d:/workspace/h2database-rust/crates/h2-sql/src/executor.rs) が **7,426 行 / 約 360 KB** に達し、SQL 実行エンジン、DCL、バックアップ、保守、外部キー、ウィンドウ関数、クエリプランナー、仮想テーブルソースが単一ファイルに混在。可読性、ナビゲーション性、保守性が著しく低下している。
   - [`crates/h2-sql/src/expression.rs`](file:///d:/workspace/h2database-rust/crates/h2-sql/src/expression.rs) も **約 2,500 行 / 約 100 KB** に達し、式評価、四則演算、文字列関数、日付/インターバル演算、JSON パス解決が一体化している。
2. **不要となった後方互換・暫定実装の残存**:
   - 新型ベクトル化集約オペレータ（`VectorizedAggregate`）導入後も残存していた旧行ベース集約プッシュダウン（`try_execute_pushdown_aggregate`: 約 125 行のデッドコード）。
   - PostgreSQL 8.x 以前の旧構文用 `legacy_options`。
   - `FETCH RELATIVE -` 等の構文前処理用暫定文字列置換。
   - 未実装の暫定スタブモジュール（`tsql/mod.rs` 等）の残存。

これらを解消し、長期的な保守性・拡張性を担保するための根本的リファクタリングを実施します。

---

## 2. リファクタリング基本原則

1. **外部 API および SQL 動作の 100% リグレッションフリー**:
   - `SQLEngine` や `Connection` 等の公開 API シグネチャ、サポートする SQL 構文、テストケースの合否に一切の破壊的変更を生じさせない。
2. **後方互換のための不要コード・デッドコードの完全撤廃**:
   - 代替機能や本番機構が完成して不要となった暫定実装・デッドコード・旧世代互換処理を徹底的に削除し、コードベースをスリム化する。
3. **責務ごとの明確なサブモジュール分割 (Single Responsibility Principle)**:
   - 1 ファイルあたりの行数を原則 1,000 行前後に抑え、ドメイン（DCL、外部キー、管理保守、クエリ計画、ウィンドウ関数等）ごとに分割する。

---

## 3. 具体的なリファクタリング計画

### 3.1 不要な後方互換・デッドコードの削除

1. **旧集約プッシュダウンの削除**:
   - `crates/h2-sql/src/executor.rs` 内の `#[allow(dead_code)] pub(crate) fn try_execute_pushdown_aggregate`（約 125 行）を削除。現在は `crates/h2-sql/src/vectorized/` で正規のベクトル集約処理が行われるため完全に不要。
2. **旧構文フォールバックの削除**:
   - `Statement::Copy` における `legacy_options` の冗長な分岐を整理し、標準の `options` 処理へ一本化。
   - `FETCH RELATIVE -` の前処理ハックを削除（sqlparser ネイティブ解釈へ統一）。
3. **未実装暫定スタブの整理**:
   - `crates/h2-sql/src/procedural/tsql/` などの空のスタブモジュールを整理。
4. **未使用ヘルパー関数の整理**:
   - `preprocess_subqueries` などの不要な `#[allow(dead_code)]` ラッパーのインライン化・削除。

---

### 3.2 `crates/h2-sql/src/executor/` へのモジュール分割

`executor.rs` を以下のディレクトリ構造に再編成します：

```text
crates/h2-sql/src/executor/
├── mod.rs             # SQLEngine 構造体、初期化、トランザクション境界、トップレベル execute
├── statement.rs       # DDL / DML 文の実行ディスパッチ (CREATE TABLE, INSERT, UPDATE, DELETE, ALTER TABLE 等)
├── query.rs           # クエリ実行パイプライン (execute_query, execute_query_with_ctes, 結合処理, EXPLAIN)
├── table_factor.rs    # テーブルソース解決 (実テーブル, VIEW, 仮想グラフテーブル, Cypher TVF, pg_proc)
├── foreign_key.rs     # 外部キー制約の検証および CASCADE UPDATE / DELETE 処理
├── window.rs          # ウィンドウ関数評価 (ROW_NUMBER, RANK, DENSE_RANK, LEAD, LAG, FIRST_VALUE, NTILE 等)
├── dcl.rs             # ユーザー・権限・セキュリティ管理 (CREATE USER, GRANT, REVOKE, 権限検査)
└── admin.rs           # 運用管理・保守 (VACUUM, ANALYZE, SHOW STATS, バックアップ/復元, SCRIPT/RUNSCRIPT)
```

各モジュールの責務：
- **`mod.rs`**:
  - `SQLEngine` のフィールド定義、`new`、`execute`、`execute_with_user`、`commit_transaction` 等のコアライフサイクル。
- **`statement.rs`**:
  - `execute_statement` の match 式。各 Statement ごとの処理（`CREATE TABLE`, `INSERT`, `UPDATE`, `DELETE`, `DROP` など）。
- **`query.rs`**:
  - SELECT クエリの評価。CTE (WITH句)、Filter、Aggregation、GROUP BY、HAVING、ORDER BY、LIMIT / OFFSET、結合（Nested Loop, Hash, Index）。
- **`table_factor.rs`**:
  - `FROM` 句に現れるテーブル要素の解決。ベーステーブルスキャン、ビュー展開、仮想グラフテーブル（`graph_x_nodes`, `graph_x_edges`）、Cypher TVF（`CYPHER(...)`）、`pg_proc`。
- **`foreign_key.rs`**:
  - `validate_foreign_keys_for_row`, `handle_foreign_keys_on_delete`, `handle_foreign_keys_on_update`。
- **`window.rs`**:
  - ウィンドウ関数のパーティショニング、ソート、各ウィンドウ関数値の計算。
- **`dcl.rs`**:
  - `execute_create_user`, `execute_alter_user`, `execute_drop_user`, `execute_grant`, `execute_revoke`, `check_statement_privileges`。
- **`admin.rs`**:
  - `execute_vacuum`, `execute_analyze`, `execute_show_stats`, `backup_to`, `restore_from`, `restore_pitr`, `script_to`, `runscript_from`, `execute_set_work_mem` 等。

---

### 3.3 `crates/h2-sql/src/expression/` へのモジュール分割

`expression.rs` を以下の構造に整理します：

```text
crates/h2-sql/src/expression/
├── mod.rs             # evaluate_expr, evaluate_expr_context, RowContext, evaluate_literal_or_unary
├── operators.rs       # 二項演算子 (evaluate_binary_op, evaluate_arithmetic_op, 比較演算, 論理演算)
└── functions.rs       # 組み込み関数 (文字列関数 substring/initcap/translate, 日付 extract/interval, JSON パス)
```

---

## 4. リグレッション検証手順

1. **モジュール分割後のビルド検証**:
   - `cargo check --workspace` でコンパイルエラーおよびモジュール可視性（`pub(crate)` 等）を確認。
2. **クレート別単体テスト**:
   - `cargo test -p h2-sql`
   - `cargo test -p h2-mvstore`
   - `cargo test -p h2`
3. **ワークスペース全体リグレッションテスト**:
   - `cargo test --workspace` を実行し、全テスト（100+ 件）がグリーンであることを確認。
