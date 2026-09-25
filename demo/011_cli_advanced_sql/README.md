# Demo 011: 専用CLI デモ 1 - 高度な SQL & クエリ演算 (Advanced SQL Suite)

H2 Database Rust の**専用対話型 CLI（`h2-cli`）**を用いて、モダン RDBMS の先進的な SQL 機能、オンライン DDL、再帰クエリ、各種ウィンドウ関数、日本語全文検索、および外部キー CASCADE 削除を一括実演するデモです。

---

## 🎯 実演している主な機能

1. **Instant / Online DDL & 並行インデックス**
   - `ALTER TABLE employees ADD COLUMN bonus ...`（メタデータ即時更新による O(1) カラム追加）
   - `CREATE INDEX idx_emp_salary ON employees (salary)`
2. **高度な DML & RETURNING 句**
   - `INSERT ... RETURNING id, name, salary`: 採番・挿入された行を即座に受取
   - `INSERT ... ON CONFLICT (id) DO UPDATE SET ...`（UPSERT 構文）
   - `UPDATE ... RETURNING ...`: 更新後の値を即時確認
3. **多彩なテーブル結合 & 集合演算子**
   - `LEFT OUTER JOIN`, `CROSS JOIN`
   - `INTERSECT`（共通集合）, `EXCEPT`（差集合）
4. **共通テーブル式 (CTE) & 再帰クエリ**
   - `WITH DeptStats AS (...)`: クエリ単位のインライン集計
   - `WITH RECURSIVE Hierarchy AS (...)`: 階層・ツリー構造の自動展開
5. **豊富なウィンドウ関数**
   - `ROW_NUMBER()`, `RANK()`, `LAG()`, `LEAD()`, `NTILE()`
6. **行値コンストラクタ**
   - `WHERE (dept_id, status) IN ((1, 'ACTIVE'), (2, 'ACTIVE'))`
7. **日本語全文検索 & JSON オブジェクト**
   - `FT_SEARCH` (2-gram 文字バイグラム検索)
   - `FT_SEARCH_MORPH` (文字種境界形態素解析検索)
   - `profile -> 'skills'`, `profile ->> 'tier'`
8. **外部キー制約 & 連鎖削除 (ON DELETE CASCADE)**
   - 親レコード削除に伴う子レコードの自動カスケード削除

---

## 🚀 実行方法

**Windows:**
```cmd
run.bat
```
または
```cmd
cargo run -p h2-cli -- -f demo/011_cli_advanced_sql/script.sql
```

**Linux / macOS:**
```bash
chmod +x run.sh
./run.sh
```

**対話型シェル内からの実行:**
```bash
cargo run -p h2-cli
h2> .read demo/011_cli_advanced_sql/script.sql
```
