# 27. 統計サンプリング更新・AutoVacuum・デッドタプルページ再利用の設計方針

> 作成日: 2026-09-26
> 関連課題: `docs/review/operation/README.md` (4. 自動最適化と統計 P0, 5. VACUUM・断片化・容量 P1)
> 参照コード: `crates/h2-sql/src/{stats.rs, executor.rs, catalog.rs}`, `crates/h2-mvstore/src/{buffer_pool/, tx/}`

---

## 1. 背景と課題

`docs/review/operation/README.md` において、運用性・信頼性観点から以下の重大課題が指摘されていた：

1. **統計情報収集の線形探索と全件走査（P0: AUTO-01〜03）**:
   - 既存の `analyze_table` は対象テーブルの全行・全列を走査し、NDV（Number of Distinct Values）や最頻値（MCV）の集計に `Vec<(Value, u64)>` の線形探索を使用していたため、高カーディナリティ列や大規模テーブルで計算量 $O(N \times \text{NDV})$ に発散するリスクがあった。
   - サンプリング機能が存在せず、変更量に応じた自動統計更新（Auto Analyze）機構も欠落していた。
2. **VACUUM が全量置換（compact）のみで通常 VACUUM が不在（P1）**:
   - `VACUUM` 実行時に全マップを走査して新規全量ファイルへ置換する重い `MVStore::compact`（PostgreSQL の `VACUUM FULL` 相当）しか存在せず、オンラインで不要行版（デッドタプル）のみを刈り取る通常 `VACUUM` がなかった。
3. **デッドタプルのみで構成されるページの再利用不能**:
   - 8KB Slotted Page および BufferPoolManager / DiskManager において、削除によってすべてのタプルが回収されたページを空きページ（Free Page）として再利用するリスト（Free Space Map / Free Page List）が存在せず、新規ページ割り当て時にファイル末尾が伸長し続ける構造となっていた。

本設計では、これらすべての課題を根本解決する。

---

## 2. アーキテクチャ全体像

```
+-----------------------------------------------------------------------------------+
|                                   h2-sql                                          |
|                                                                                   |
|  +--------------------+        +---------------------+      +------------------+  |
|  | AutoVacuum / Auto  |  --->  | Sampling Statistics | ---> | Catalog TableDef |  |
|  | Analyze 調整器     |        | 更新 (stats.rs)     |      | TableStats 永続化|  |
|  +--------------------+        +---------------------+      +------------------+  |
|            |                                                                      |
|            | (Vacuum 指示)                                                        |
+------------|----------------------------------------------------------------------+
             v
+-----------------------------------------------------------------------------------+
|                                  h2-mvstore                                       |
|                                                                                   |
|  +--------------------------+         +----------------------------------------+  |
|  | MVCC Dead Tuple Pruning  |  <--->  | SlottedPage / BufferPoolManager        |  |
|  | (最古トランザクションより|         | - is_all_dead() 判定                   |  |
|  |  古い削除版の物理破棄)   |         | - mark_as_free() & Free Page List 登録 |  |
|  +--------------------------+         +----------------------------------------+  |
|                                                           |                       |
|                                                           v                       |
|                                       +----------------------------------------+  |
|                                       | DiskManager Free Page 再利用           |  |
|                                       | (allocate_page は free_pages を優先)   |  |
|                                       +----------------------------------------+  |
+-----------------------------------------------------------------------------------+
```

---

## 3. 各コンポーネントの設計詳細

### 3.1 統計情報のサンプリング更新 (`h2-sql::stats`)

1. **サンプリングアルゴリズム**:
   - テーブル全行数 $N$ に対し、目標サンプル行数 $S$ を決定（デフォルト 3,000〜30,000 行、または SQL の `WITH SAMPLE n [PERCENT|ROWS]` で指定）。
   - $N \le S$ の場合は全行スキャン、 $N > S$ の場合は一定歩進（Systematic Stride: $k = \lceil N / S \rceil$）による高速サンプリングを実施。
2. **ハッシュベースの高速頻度集計**:
   - 従来の線形探索 `Vec<(Value, u64)>` を廃止し、`HashMap<Value, u64>`（$O(1)$）による重複カウントを採用。
3. **NDV および分布推計**:
   - サンプル集計値から母集団の推定 NDV をスケール算出（$NDV_{\text{est}} = \min(N, NDV_{\text{sample}} \times \text{scale})$）。
   - 上位 10 件の MCV（Most Common Values）および出現頻度、NULL 率（null_frac）、平均バイト幅（avg_width）を算出。
   - `TableStats` に `sample_ratio`（サンプリング率）を記録し永続化。

### 3.2 デッドタプル回収と空きページ再利用 (`h2-mvstore::buffer_pool`)

1. **SlottedPage の全デッド判定と空き化**:
   - `live_tuple_count(buf)`: 有効タプル数（offset > 0 かつ len > 0）を算出。
   - `is_all_dead(buf)`: ページ内の全タプルが削除済み、あるいは `tuple_count == 0` であるか判定。
   - `mark_as_free(buf)`: `page_type = 2 (Free)` に設定し、スロットおよびヘッダを初期化。
2. **DiskManager Free Page List（空きページ管理）**:
   - `DiskManager` 内に `free_pages: Mutex<Vec<u32>>` を追加。
   - `allocate_page()` 呼び出し時、`free_pages` に空きページが存在すればそれを優先して再利用（ファイル伸長を抑制）。
   - 空きページがない場合のみ `num_pages` をインクリメント。
   - `deallocate_page(page_id)` で再利用可能リストへ返却。
3. **BufferPoolManager との連携**:
   - `free_page(page_id)`: バッファフレームを初期化し、`DiskManager::deallocate_page` へ登録。
   - `vacuum_page(page_id)`: ページ内がすべてデッドタプルの場合は `free_page` を行い、生存タプルが存在する場合はインプレース・デフラグ（`defragment`）で空き領域を前方に統合。

### 3.3 MVCC レベルの不要行版回収（通常 VACUUM）

1. **MVMap / VersionedValue の Tombstone 刈り取り**:
   - 稼働中トランザクションの最小スナップショットバージョン $V_{\min}$ を取得。
   - 最新コミット履歴が `value: None`（削除済み Tombstone）であり、かつ `commit_version \le V_{\min}` のキーは、どのトランザクションからも不可視であることが保証されるため、マップから完全に削除。
   - 複数世代にわたる履歴のうち、$V_{\min}$ より古い過去世代を刈り込み（Prune）。

### 3.4 PostgreSQL 準拠 AutoVacuum & AutoAnalyze 調整器

1. **テーブル変更量の自動追跡**:
   - 各テーブルの INSERT / UPDATE / DELETE 発生時に、更新行数（`mod_count`）およびデッドタプル増加量（`dead_tuples`）をインメモリ集計。
2. **発火しきい値判定式**:
   $$\text{Threshold}_{\text{vacuum}} = \text{vacuum\_base\_threshold} + (\text{vacuum\_scale\_factor} \times \text{row\_count})$$
   $$\text{Threshold}_{\text{analyze}} = \text{analyze\_base\_threshold} + (\text{analyze\_scale\_factor} \times \text{row\_count})$$
   - デフォルト値:
     - `vacuum_base_threshold`: 50 行, `vacuum_scale_factor`: 0.20 (20%)
     - `analyze_base_threshold`: 50 行, `analyze_scale_factor`: 0.10 (10%)
3. **自動保守トリガー**:
   - 変更量がしきい値を超過したテーブルに対し、バックグラウンドまたはクエリコミット境界で自動 `ANALYZE`（サンプリング統計更新）および `VACUUM`（行版刈り取り・空きページ回収）を実行。
4. **手動コマンド拡張**:
   - `VACUUM [table_name]`: 通常 VACUUM（デッドタプル・空きページ回収）
   - `VACUUM FULL`: 全量コンパクション（全マップ再構築・物理ファイル圧縮）
   - `ANALYZE [table_name] [WITH SAMPLE <n> [PERCENT|ROWS]]`
   - `SHOW STATS [table_name]`: 統計情報およびサンプリング率の確認
   - `SHOW AUTOVACUUM`: 自動バキューム調整器の状態およびテーブル別変更累積の確認

---

## 4. 期待される効果と検証方針

1. **統計収集速度**: 大規模テーブルにおいて全行 $O(N \times \text{NDV})$ から $O(S)$ への短縮（サンプリング $S=3,000$ で数十倍以上の高速化）。
2. **ストレージ再利用**: 大量削除後、`VACUUM` または AutoVacuum によってデッドページが `Free Page List` に登録され、その後の INSERT で新規ページを確保せず既存空きページが再利用されることをユニットテストで検証。
3. **自動最適化**: 大量データ更新後に自動で `ANALYZE` が発火し、`last_analyzed` と統計情報が更新されることを検証。
