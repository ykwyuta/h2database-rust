# 10. 統計情報収集・更新機構の調査報告

> 調査日: 2026-09-25
> 調査対象ブランチ: `main`
> 対象コード: `crates/h2-sql/src/{catalog.rs, executor.rs}`, `crates/h2-mvstore/src/`

---

## 1. 調査サマリー

**結論: 現行実装には統計情報収集・更新機構が一切存在しない。**

テーブル行数・カラムカーディナリティ・ヒストグラム・NDV（Number of Distinct Values）といったクエリオプティマイザが必要とする統計情報は、カタログにも MVStore にも保持されていない。
これはコストベース最適化が不可能な状態であることを意味し、JOIN 順序選択・インデックス利用判断・集約処理の最適化などが一切コストに基づかず行われていることを示している。

---

## 2. 調査対象と調査手法

| 調査対象 | 手法 |
|---|---|
| `crates/h2-sql/src/catalog.rs` | 構造体・フィールド全列挙 |
| `crates/h2-sql/src/executor.rs` | `fn explain_statement`, `fn execute_query`, インデックス選択ロジックの精読 |
| `crates/h2-mvstore/src/` | ストレージ層で統計値が永続化されているか確認 |
| 全 `.rs` ファイル | キーワード検索: `statistics`, `cardinality`, `selectivity`, `histogram`, `ndistinct`, `row_count`, `table_stats`, `analyze`, `cost`, `estimate`, `planner`, `optimizer` |

すべての検索で **0 件** が返却された。

---

## 3. 現行カタログ構造の詳細

### 3.1 `TableDef` — 統計フィールドなし

```rust
// crates/h2-sql/src/catalog.rs (L106-L124)
pub struct TableDef {
    pub name: String,
    pub schema: String,
    pub columns: Vec<ColumnDef>,
    pub next_row_id: u64,        // 挿入カウンタ（行数ではない）
    pub foreign_keys: Vec<ForeignKeyDef>,
    pub primary_key: Vec<String>,
    pub unique_constraints: Vec<UniqueConstraintDef>,
    pub is_queue: bool,
    pub retention_duration_ms: Option<u64>,
    pub max_bytes: Option<u64>,
}
```

`next_row_id` は INSERT のたびにインクリメントされる単調増加 ID であり、DELETE されても減らない。**現在の正確な行数を返す方法が存在しない**。

統計情報として期待される以下のフィールドが一切存在しない:

| PostgreSQL `pg_statistic` 相当 | SQL Server `sys.dm_db_stats_properties` 相当 | h2-database-rust |
|---|---|---|
| `n_distinct` (NDV) | `rows_sampled` | なし |
| `correlation` | `modification_counter` | なし |
| MCV リスト (頻出値) | ヒストグラムステップ数 | なし |
| `n_tup_ins / n_tup_upd / n_tup_del` | `last_updated` | なし |

### 3.2 `IndexDef` — サイズ・深さ情報なし

```rust
// crates/h2-sql/src/catalog.rs (L250-L255)
pub struct IndexDef {
    pub name: String,
    pub table_name: String,
    pub columns: Vec<String>,
    pub is_unique: bool,
}
```

B-Tree の深さ・リーフノード数・エントリ数・クラスタリングファクタなどが一切格納されていない。

---

## 4. EXPLAIN の実装状況

`EXPLAIN` は実装されているが、**コスト見積もりを一切行わない純粋な構文ツリー整形**にすぎない。

```rust
// crates/h2-sql/src/executor.rs (L3107-L3191)
fn explain_statement(&self, _tx: &Transaction, stmt: Statement) -> H2Result<String> {
    // スキャン種別判定（等値条件のみ、コスト比較なし）
    let mut scan_type = format!("TableScan: {}", base_table_name);
    if let Some(Expr::BinaryOp { left, op: BinaryOperator::Eq, .. }) = &select.selection {
        if let Expr::Identifier(ident) = left.as_ref() {
            let col_name = &ident.value;
            let indexes = self.catalog.get_table_indexes(&base_table_name);
            if let Some(target_idx) = indexes.iter().find(|i|
                !i.name.starts_with("pk_") && i.columns[0].eq_ignore_ascii_case(col_name)
            ) {
                scan_type = format!("IndexScan: {} on index {}", base_table_name, target_idx.name);
            }
        }
    }
    // JOIN は無条件に NestedLoopJoin と表示
    for join in &from_table.joins {
        lines.push(format!("NestedLoopJoin: {}", join_tbl));
    }
}
```

EXPLAIN が出力する情報:

| 出力項目 | 実際の動作 |
|---|---|
| `TableScan / IndexScan` | 等値条件のカラム名がインデックス定義先頭カラムと一致すれば IndexScan |
| `NestedLoopJoin` | 全 JOIN に無条件表示（実アルゴリズムではなく文字列のみ） |
| `Filter` | WHERE 句をそのまま文字列化 |
| `Sort` | ORDER BY をそのまま文字列化 |
| **コスト (cost=)** | **出力されない** |
| **実行時間 (actual time=)** | **出力されない** |
| **推定行数 (rows=)** | **出力されない** |

---

## 5. インデックス選択ロジック（ルールベース）

実際のクエリ実行でもインデックス選択はルールベース:

```rust
// crates/h2-sql/src/executor.rs (L3740-L3785)
let mut index_scanned: Option<Vec<Row>> = None;
if from_table.joins.is_empty() {   // JOIN があればインデックスは使わない
    if let Some(Expr::BinaryOp { left, op: BinaryOperator::Eq, right }) = &select.selection {
        if let Expr::Identifier(ident) = left.as_ref() {
            let col_name = &ident.value;
            let indexes = self.catalog.get_table_indexes(&base_table_name);
            // 先頭カラムが一致する最初のインデックスを無条件採用
            if let Some(target_idx) = indexes.iter().find(|i|
                i.columns[0].eq_ignore_ascii_case(col_name)
            ) { /* インデックススキャン実行 */ }
        }
    }
}
// それ以外はフルスキャン
let rows = if let Some(r) = index_scanned { r } else {
    let entries = tx.scan_visible(&map_name)?;  // 全件取得
    // ...
};
```

インデックス選択の判定基準:

| 条件 | 現行の判定 |
|---|---|
| WHERE 句が `=` 等値比較のみ | インデックス候補を探索 |
| WHERE 句が `>`, `<`, `BETWEEN` | **フルスキャン強制** |
| JOIN が存在する | **フルスキャン強制** |
| 複数インデックスが候補の場合 | **最初に見つかったものを使用（コスト比較なし）** |
| インデックスが使えるがテーブルが小さい | **判断できない（行数が不明）** |

---

## 6. 主要 RDBMS との比較

### 6.1 PostgreSQL

| 機能 | PostgreSQL | h2-database-rust |
|---|---|---|
| テーブル行数推定 | `pg_class.reltuples` | なし |
| NDV (列カーディナリティ) | `pg_statistic.stadistinct` | なし |
| MCV リスト | `pg_statistic.stanumbers` | なし |
| ヒストグラム境界値 | `pg_statistic.stavalues` (等高ヒストグラム) | なし |
| 自動統計更新 | autovacuum ANALYZE | なし |
| 手動統計更新 | `ANALYZE [tablename]` | なし |
| コストモデル | `seq_page_cost`, `random_page_cost`, `cpu_tuple_cost` | なし |
| JOIN アルゴリズム選択 | Hash Join / Merge Join / Nested Loop をコスト比較 | 固定: Nested Loop のみ |

### 6.2 SQL Server

| 機能 | SQL Server | h2-database-rust |
|---|---|---|
| テーブル行数推定 | `sys.partitions.rows` | なし |
| 自動統計更新 | `AUTO_UPDATE_STATISTICS = ON` | なし |
| Filtered Statistics | 部分インデックスの統計 | なし |
| Cardinality Estimator | CE70 / CE120 / CE150 | なし |
| Parameter Sniffing | 実行計画の再利用とフラッシュ | なし |

### 6.3 H2 (Java 版、参考)

| 機能 | H2 (Java) | h2-database-rust |
|---|---|---|
| `ANALYZE` 文 | ○（テーブル統計更新） | なし |
| `TABLE_ROWS` (information_schema) | ○ | なし |
| コストベース結合順序選択 | ○（貪欲法） | なし |
| 選択率推定 | ○（簡易） | なし |

---

## 7. 問題点の整理

### 問題 1: 行数が取得できない

`SELECT COUNT(*) FROM t` はフルスキャンで実行される。`TableDef.next_row_id` は DELETE 後も増加し続けるため近似値にもならない。

- オプティマイザが JOIN の小テーブル側を選べない
- `LIMIT n` があっても、フルスキャン後に切り捨てる実装になっている可能性が高い

### 問題 2: インデックスの絞り込み効果が不明

カーディナリティ情報がないため「インデックスを使う vs フルスキャン」の選択が純粋にルールベース（等値条件の有無のみ）。  
例えば `WHERE status = 'active'` でほぼ全行が active の場合でも、インデックスが採用されてしまう。

### 問題 3: JOIN 順序が固定

FROM 句と JOIN 句の記述順序のまま Nested Loop Join が実行される。小テーブルを外側、大テーブルを内側にするなどの最適化ができない。

### 問題 4: RANGE スキャン・LIKE のインデックス活用が未実装

`BETWEEN`, `>`, `<`, `LIKE 'prefix%'` などの範囲条件でインデックスが使用されない。

---

## 8. 改善案・ロードマップ

> **前提:** メモリ管理ドキュメント (`09_memory_management_architecture_and_comparison.md`) のロードマップと整合させ、**Phase 2 の Buffer Pool 実装完了後**に統計情報機構を導入することを推奨する。Buffer Pool が完成しないとディスクI/Oコストの見積もりができないためである。

### Step 1: カタログへの基礎統計の追加（最小コスト）

```rust
pub struct TableStats {
    pub row_count: u64,             // 近似行数（INSERT+1, DELETE-1）
    pub dead_row_count: u64,        // 削除済み行数
    pub last_analyzed: Option<u64>, // Unix timestamp (ms)
    pub total_pages: u64,           // 将来の Page ベースストレージに備える
}

pub struct ColumnStats {
    pub ndv: u64,                        // Number of Distinct Values
    pub null_frac: f64,                  // NULL 率 (0.0-1.0)
    pub avg_width: f64,                  // 平均バイト幅
    pub most_common_vals: Vec<Value>,    // 頻出値（Top-N）
    pub most_common_freqs: Vec<f64>,     // 頻出値の頻度
}
```

### Step 2: `ANALYZE` 文の実装

```sql
ANALYZE [tablename];   -- 特定テーブル
ANALYZE;               -- 全テーブル
```

- フルスキャンして行数・NDV・NULL 率・MCV を計算
- サンプリング率を設定可能にする（デフォルト 10%、小テーブルはフルスキャン）
- 結果を `TableDef.stats` / `ColumnDef.stats` に書き込み永続化

### Step 3: 選択率推定関数の追加

```rust
fn estimate_selectivity(col_stats: &ColumnStats, pred: &Expr) -> f64 {
    match pred {
        BinaryOp { op: Eq, right } => {
            // MCV に含まれるなら実測頻度を使用、それ以外は 1.0 / ndv
        }
        BinaryOp { op: Gt | Lt | Between, .. } => {
            // ヒストグラムがあれば範囲の面積で推定、なければ 1/3
        }
        IsNull => col_stats.null_frac,
    }
}
```

### Step 4: コストベース JOIN 順序選択

```rust
// 貪欲法による JOIN 順序選択（PostgreSQL の GEQO オフ時相当）
fn choose_join_order(tables: &[TableRef], stats: &CatalogStats) -> Vec<TableRef> {
    // 各テーブルの推定行数を取得
    // 最小行数のテーブルをドライビングテーブルに
    // 結合後の行数を選択率で推定しながら順序を決定
}
```

### Step 5: Range Scan のインデックス活用

```rust
match op {
    BinaryOperator::Eq => { /* 既存ロジック */ }
    BinaryOperator::Gt | BinaryOperator::GtEq
    | BinaryOperator::Lt | BinaryOperator::LtEq => {
        // B-Tree の range scan API を呼び出す
    }
}
```

### 実装優先度マトリクス

| ステップ | 実装コスト | 効果 | 推奨時期 |
|---|---|---|---|
| Step 1: カタログ拡張（行数カウンタ） | 低 | 中（`COUNT(*)` 高速化） | **今すぐ可能** |
| Step 2: `ANALYZE` 文 | 中 | 高（以降のステップの前提） | Phase 2 完了後 |
| Step 3: 選択率推定 | 中 | 高（インデックス選択精度向上） | Phase 2 完了後 |
| Step 4: JOIN 順序選択 | 高 | 高（複数テーブル JOIN の劇的改善） | Phase 3 完了後 |
| Step 5: Range Scan | 中 | 高（BETWEEN/< > クエリ改善） | **今すぐ可能** |

---

## 9. 即座に対応可能な暫定改善

Phase 2 を待たずに実装できる最小コストの改善:

### 9.1 行数の近似カウンタ

```rust
// TableDef に追加（既存フィールドを壊さない）
#[serde(default)]
pub approx_row_count: i64,   // INSERT +1, DELETE -1, TRUNCATE 0 にリセット
```

### 9.2 Range Scan サポート（インデックス選択の拡張）

等値条件マッチを範囲条件まで拡張する（統計不要で実装可能）。
B-Tree は本来範囲スキャンをサポートするため、MVStore の API を range scan モードで呼び出すだけで対応できる可能性がある。

### 9.3 information_schema.TABLES の TABLE_ROWS 列

現在 `information_schema.TABLES` が `NULL` を返す `TABLE_ROWS` 列を `approx_row_count` で埋める。

---

## 10. 調査時の評価まとめ（実装前）

| 評価項目 | 調査時点 |
|---|---|
| 統計情報収集機構 | 未実装 |
| 統計情報格納フィールド | 未実装 |
| `ANALYZE` 文 | 未実装 |
| コストベース最適化 | 未実装（全てルールベース） |
| インデックス選択 | 等値条件のみ、コスト比較なし |
| JOIN 順序選択 | SQL 記述順に固定 |
| JOIN アルゴリズム | Nested Loop のみ |
| EXPLAIN コスト出力 | 未実装（構文情報のみ） |

---

## 11. 統計情報基盤の実装完了報告 (2026-09-25)

上記の調査結果およびロードマップに基づき、**統計情報基盤およびコストベース最適化の完全実装が完了**しました。

### 11.1 実装内容

1. **カタログへの統計情報構造体追加 (`crates/h2-sql/src/catalog.rs`)**:
   - `TableStats`: 行数（`row_count`）、削除行数（`dead_row_count`）、最終解析時刻（`last_analyzed`）、推定ページ数（`total_pages`）。
   - `ColumnStats`: NDV（`ndv`）、NULL率（`null_frac`）、平均幅（`avg_width`）、頻出値 Top 10（`most_common_vals`）および頻度（`most_common_freqs`）。
   - `TableDef` に `stats: Option<TableStats>` と `approx_row_count: i64` を追加。
   - `ColumnDef` に `stats: Option<ColumnStats>` を追加。
   - `INSERT` 時に `+affected_rows`、`DELETE` 時に `-affected_rows`、`TRUNCATE` 時に `0` に自動更新。
2. **`ANALYZE` 文の実装 (`crates/h2-sql/src/stats.rs`, `executor.rs`)**:
   - `ANALYZE [table_name]` および `ANALYZE`（全テーブル）構文の実行をサポート。
   - テーブル内の全タプルを走査し、正確な行数・NULL率・ユニーク値数（NDV）・頻出値 Top 10・平均バイト幅・8KB換算ページ数を集計してカタログへ永続化。
3. **選択率推定（Selectivity Estimation）とコストモデル (`crates/h2-sql/src/stats.rs`)**:
   - 等値条件（MCV または 1/NDV）、不等号条件（`<, <=, >, >=`）、`BETWEEN`、`IS NULL`、`AND`、`OR` の選択率を数理的に推定。
   - `estimate_scan_cost`: シーケンシャルスキャン（ページI/O + 行評価コスト）とインデックススキャン（インデックスI/O + ランダム行アクセス）のコストを計算。
4. **Range Scan インデックス活用 (`crates/h2-sql/src/executor.rs`)**:
   - 従来の `=` 等値比較だけでなく、`<`, `<=`, `>`, `>=`, `BETWEEN` でもインデックスを活用したスキャンを実行。
5. **EXPLAIN 出力のコスト・行数表示**:
   - `TableScan: table (cost=X.XX rows=N)`
   - `IndexScan: table on index idx (cost=X.XX rows=N)`
   - 推定コストと推定行数を詳細に表示。
6. **`information_schema.TABLES` の `TABLE_ROWS` 列追加**:
   - 統計または近似行数をリアルタイムに参照可能。
