# SQL 標準規格 適合状況と機能比較 (SQL Standards Compliance)

本ドキュメントは、**H2 Database in Rust (`h2database-rust`)** における SQL 構文および機能の実装状況を、**国際標準規格（ISO/IEC 9075: SQL-92, SQL:1999, SQL:2003, SQL:2008, SQL:2016）** と対比して体系的に整理・明記したものです。

アプリケーション設計時や既存データベース（SQLite, PostgreSQL, Java版 H2 Database）からの移行検討時に「何が利用でき、何がまだ利用できないか」の明確な指標としてご活用ください。

---

## 📊 1. 全体サマリー (High-Level Summary)

| 分野 | 主な標準規格 | 実装完了 (Supported) | 未実装 / 制限事項 (Unsupported / Limitations) |
| :--- | :--- | :--- | :--- |
| **データ型** | SQL-92 / SQL:1999 / SQL:2016 | 真偽値, 整数各種, 浮動小数, 高精度数値(Decimal), 文字列(Varchar/Text), バイナリ, 日付時刻(Date/Time/Timestamp), **TIMESTAMPTZ (タイムゾーン保持型)**, UUID, JSON/JSONB, 配列 | `INTERVAL`, `ENUM`, 複合型/ユーザー定義型(UDT), 空間型 |
| **DDL (定義)** | SQL-92 / SQL:2008 | `CREATE TABLE` (PK, Not Null, **Foreign Key/参照整合性**), `DROP TABLE`, **`ALTER TABLE` (Instant Add Col, Instant Drop Col, Online Rename Table)**, **`TRUNCATE TABLE` (Online Truncate)**, **`CREATE INDEX CONCURRENTLY` (Online Index)**, `DROP INDEX`, **`CREATE/DROP VIEW` (仮想ビュー)**, **`VACUUM` (Concurrent Vacuum)** | `CHECK` 制約, 複合主キー制約, `CREATE SCHEMA` |
| **DML (操作)** | SQL-92 / SQL:2003 | 単行/複数行 `INSERT`, **`INSERT INTO ... SELECT`**, `UPDATE` (複数列代入・自己参照式・FK検証), `DELETE` (連動削除 CASCADE/SET NULL/RESTRICT) | `UPSERT` (`ON CONFLICT DO UPDATE`), `RETURNING` 句 |
| **DQL (検索)** | SQL-92 / SQL:1999 / SQL:2003 | FROM なし `SELECT`, 列射影・エイリアス, **共通テーブル式 (`WITH` / CTE)**, `WHERE`, `IN`, `BETWEEN`, `CASE WHEN`, `JOIN` (Inner, Left Outer, View/Chained Join), `GROUP BY`, `HAVING`, `ORDER BY`, `LIMIT/OFFSET`, `DISTINCT`, **集合演算 (`UNION`, `INTERSECT`, `EXCEPT` / ALL)**, **ウィンドウ関数 (`ROW_NUMBER`, `RANK`, `DENSE_RANK`)**, サブクエリ (Derived Table, IN, EXISTS, スカラ) | 再帰 CTE (`WITH RECURSIVE`), `RIGHT/FULL OUTER JOIN`, `CROSS JOIN` |
| **TCL (トランザクション)** | SQL-92 | `BEGIN`, `COMMIT`, `ROLLBACK`, MVCC スナップショット分離, 自動 Undo Log 復元, **デッドロック検出・自動キャンセル (Victim Rollback)**, **クエリ単位実行タイムアウト (`SET statement_timeout`)** | **`SAVEPOINT` (設計方針として実装対象外)**, 動的分離レベル変更 (`SET TRANSACTION ISOLATION LEVEL`) |
| **関数・演算子** | SQL-92 / 拡張 | `COUNT`, `SUM`, `AVG`, `MIN`, `MAX`, `COALESCE`, `UPPER`, `LOWER`, `CONCAT`, `LENGTH`, `ABS`, `NOW`, JSON 演算子 (`->`, `->>`), 日本語全文検索 (`FT_SEARCH`) | 三角関数/指数関数, 正規表現関数 (`REGEXP`), 日付間隔演算 (`DATE_ADD`) |
| **メタデータ・診断** | SQL-92 / SQL:2008 / MySQL互換 | `INFORMATION_SCHEMA.TABLES`, `INFORMATION_SCHEMA.COLUMNS`, `SHOW TABLES`, `SHOW COLUMNS`, `EXPLAIN` | `INFORMATION_SCHEMA.VIEWS / CONSTRAINTS`, `EXPLAIN ANALYZE` (実測プロファイリング) |

---

## 🗃️ 2. データ型 (Data Types)

本データベースは、厳格な静的型付けと実行時バリデーションを備えています。

### 実装できている機能 (Supported)
- **真偽値型**: `BOOLEAN`, `BOOL` (`TRUE`, `FALSE`)
- **符号付き整数型**:
  - `TINYINT` (8-bit: -128 〜 127)
  - `SMALLINT` (16-bit: -32,768 〜 32,767)
  - `INT`, `INTEGER` (32-bit: -2,147,483,648 〜 2,147,483,647)
  - `BIGINT` (64-bit: -9,223,372,036,854,775,808 〜 9,223,372,036,854,775,807)
- **浮動小数点数型**:
  - `REAL`, `FLOAT` (32-bit IEEE 754 単精度)
  - `DOUBLE`, `DOUBLE PRECISION` (64-bit IEEE 754 倍精度)
- **固定小数点・高精度数値型 (SQL-92)**:
  - `DECIMAL(p, s)`, `NUMERIC(p, s)` (金融・通貨計算用の 128-bit 厳密小数、誤差ゼロ)
- **文字列型**:
  - `VARCHAR(n)` (可変長文字列)
  - `CHAR(n)` (固定長文字列)
  - `TEXT` / `CLOB` (無制限テキスト)
- **バイナリ型**:
  - `BINARY(n)`, `VARBINARY(n)`, `BLOB`, `BYTEA`
- **日付・時刻型**:
  - `DATE` (年-月-日: `YYYY-MM-DD`)
  - `TIME` (時:分:秒: `HH:MM:SS`)
  - `TIMESTAMP` (日時: `YYYY-MM-DD HH:MM:SS`)
  - **`TIMESTAMPTZ` / `TIMESTAMP WITH TIME ZONE`**:
    - タイムゾーンオフセット（`+09:00`, `-05:00`, `Z` 等）を保持・解析し、内部で UTC に正規化して正確な比較・格納を実施。
- **最新標準・拡張型**:
  - `UUID` (128-bit RFC 4122 準拠)
  - `JSON`, `JSONB` (バイナリ JSON 構造化データ)
  - `ARRAY` (同種要素のリスト)

### 実装できていない機能・制限事項 (Unsupported / Limitations)
- ❌ **時間間隔型 (`INTERVAL`)**:
  - `INTERVAL '1 day'` などの期間表現型および日付演算構文は未実装。
- ❌ **明示的シリアル型 (`SERIAL`, `BIGSERIAL`, `GENERATED ALWAYS AS IDENTITY`)**:
  - 内部的には 64-bit Row ID による自動採番機構が稼働していますが、DDL 構文としての `SERIAL` や明示的な `SEQUENCE` オブジェクト構文（`CREATE SEQUENCE`）は未実装。
- ❌ **ユーザー定義型・列挙型 (`CREATE TYPE ... AS ENUM`)**:
  - 独自ドメイン型や ENUM 型の定義は未対応（`VARCHAR` + アプリケーション層または `IN` 述語で代替）。
- ❌ **空間型 (GIS / Spatial)**:
  - `GEOMETRY`, `POINT`, `POLYGON` などの地理空間型は未実装。

---

## 🏗️ 3. データ定義言語 (DDL - Data Definition Language)

### 実装できている機能 (Supported)
- **テーブル作成 (`CREATE TABLE`)**:
  - `CREATE TABLE [IF NOT EXISTS] tbl (col type, ...)`
  - 単一列主キー制約: `PRIMARY KEY`
  - 必須制約: `NOT NULL`
  - **外部キー制約 (`FOREIGN KEY ... REFERENCES ...`)**:
    - 列レベル定義: `col INT REFERENCES parent_tbl(id)`
    - テーブル制約レベル定義: `FOREIGN KEY (col) REFERENCES parent_tbl(id) [ON DELETE ...] [ON UPDATE ...]`
    - 連動削除・更新アクション: `CASCADE`（連動削除/連動更新）、`SET NULL`（NULL化）、`RESTRICT` / `NO ACTION`（削除・更新拒否）
    - NULL値の許容（SQL標準準拠：外部キー列がNULLの場合は制約違反とならない）
    - 被参照テーブルの保護（子行が存在する場合の `DROP TABLE` および `TRUNCATE TABLE` を自動拒絶）
- **テーブル削除 (`DROP TABLE`)**:
  - `DROP TABLE [IF EXISTS] tbl`
  - 削除時に所属するセカンダリインデックスおよびストレージマップも安全に物理破棄。
- **テーブル定義変更 (`ALTER TABLE`) - 完全オンライン / Instant DDL**:
  - **オンライン・テーブル名変更**: `ALTER TABLE tbl RENAME TO new_tbl` (内部マップのキー置換のみで $O(1)$ アトミックに完了)
  - **インスタント・列追加**: `ALTER TABLE tbl ADD [COLUMN] col_name data_type` (全行物理書き換えを行わず $O(1)$ メタデータ更新。読み出し時に NULL 透過補完)
  - **インスタント・列削除**: `ALTER TABLE tbl DROP [COLUMN] col_name [IF EXISTS]` (全行物理削除ループを行わず $O(1)$ カタログ更新。射影時に自動非表示)
- **全データ高速削除 (`TRUNCATE TABLE`) - 完全オンライン**:
  - `TRUNCATE TABLE tbl` (1行ずつの削除ループを廃止し、B-Tree ツリーを $O(1)$ 一括クリア。Row ID を 1 に初期化)
- **インデックス管理 (`CREATE INDEX`, `DROP INDEX`)**:
  - `CREATE [UNIQUE] INDEX [CONCURRENTLY] [IF NOT EXISTS] idx_name ON tbl (col)`: **Online Index Build** 対応（スナップショット分離により並行ライターをブロックせず構築可能）
  - `DROP INDEX [IF EXISTS] idx_name`
  - B-Tree による O(log N) 探索、一意性制約の強制。
- **オンライン・ストレージコンパクション (`VACUUM`)**:
  - `VACUUM`: **Concurrent Vacuum** 対応（並行トランザクションをブロックせず、未コミット変更を排除してコミット済みデータのみを一時ファイル書き出し＋アトミック置換してファイルサイズを物理縮小）
- **仮想ビュー管理 (`CREATE VIEW`, `DROP VIEW`)**:
  - `CREATE VIEW view_name AS query`: 仮想ビューの定義とカタログ永続化
  - `CREATE OR REPLACE VIEW view_name AS query`: 既存ビューの安全な置換
  - 列名明示指定: `CREATE VIEW view_name (col1, col2) AS query`
  - `DROP VIEW [IF EXISTS] view_name`: ビュー定義の削除
  - クエリ実行時の動的ビュー展開、テーブルとの `JOIN`、CTE との併用、多段ビュー結合に完全対応。
- **複合主キー・複合一意制約**:
  - `CREATE TABLE tbl (..., CONSTRAINT pk PRIMARY KEY (col1, col2))`
  - `CREATE TABLE tbl (..., CONSTRAINT uq UNIQUE (col1, col2))`
  - テーブル制約構文での複合主キーおよび複合ユニーク制約の完全サポート。
  - カタログ永続化、自動インデックス構築、挿入・更新時の複合キー一意性チェックに対応。
- **スキーマ管理 (`CREATE SCHEMA`, `DROP SCHEMA`)**:
  - `CREATE SCHEMA [IF NOT EXISTS] schema_name`: スキーマの作成とカタログ永続化
  - `DROP SCHEMA [IF EXISTS] schema_name [CASCADE | RESTRICT]`: スキーマの安全な削除および配下テーブルの一括破棄
  - `schema.table` 形式の修飾テーブル名解決、スキーマごとのテーブル・インデックス分離に対応。

### 実装できていない機能・制限事項 (Unsupported / Limitations)
- ❌ **CHECK 制約 (`CHECK (expr)`)**:
  - 任意式による行バリデーションは未実装。
- ❌ **マテリアライズドビュー (`CREATE MATERIALIZED VIEW`)**:
  - クエリ結果を物理的に保持・リフレッシュするマテリアライズドビューは未実装（仮想ビューは完全サポート）。
- ❌ **列定義の変更 (`ALTER COLUMN ... TYPE ...`, `RENAME COLUMN`)**:
  - 既存列のデータ型変換や列名のリネームは未実装（新規列追加 → 移行 → 旧列削除で代替）。

---

## ✏️ 4. データ操作言語 (DML - Data Manipulation Language)

### 実装できている機能 (Supported)
- **行挿入 (`INSERT INTO`)**:
  - `INSERT INTO tbl [(col, ...)] VALUES (val, ...), (val, ...)`
  - 複数行（バルク）挿入に対応。
  - 列定義への自動型キャスト、外部キー参照整合性の自動検証、一意インデックスの重複検証。
- **クエリ結果の直接挿入 (`INSERT INTO ... SELECT ...`)**:
  - `INSERT INTO tbl [(col, ...)] SELECT ...`
  - 全列一括転送、指定列と計算式の転送、集約クエリ結果（`GROUP BY`）の直接挿入、CTE（`WITH` 句）を伴う挿入に対応。
- **行更新 (`UPDATE`)**:
  - `UPDATE tbl SET col1 = expr1, col2 = expr2 [WHERE cond]`
  - 自己参照更新（例: `SET balance = balance - 100`）に対応。
  - 更新に伴うインデックスキーの自動再配置、外部キー整合性の検証、および親行更新時の `ON UPDATE CASCADE / SET NULL / RESTRICT` 連動。
- **行削除 (`DELETE`)**:
  - `DELETE FROM tbl [WHERE cond]`
  - 削除に伴うインデックスキーの自動クリーンアップ、および子テーブルへの `ON DELETE CASCADE / SET NULL / RESTRICT` 連動。
- **条件付き挿入・更新 (`UPSERT` / `MERGE`)**:
  - `INSERT INTO tbl (...) VALUES (...) ON CONFLICT (col1, ...) DO NOTHING`
  - `INSERT INTO tbl (...) VALUES (...) ON CONFLICT (col1, ...) DO UPDATE SET col = expr` (`EXCLUDED.col` 擬似テーブル参照対応)
  - `MERGE INTO target USING source ON cond WHEN MATCHED THEN UPDATE SET ... WHEN NOT MATCHED THEN INSERT (...) VALUES (...)`
- **DML 戻り値句 (`RETURNING`)**:
  - `INSERT INTO tbl (...) VALUES (...) RETURNING id, col, expr`
  - `UPDATE tbl SET ... WHERE ... RETURNING *`
  - `DELETE FROM tbl WHERE ... RETURNING *`
  - DML 実行と同時に影響を受けた行のプロジェクション結果をクライアントに返却。
- **結合を伴う更新・削除 (`UPDATE ... FROM`, `DELETE ... USING`)**:
  - `UPDATE tbl SET col = f.val FROM other f WHERE tbl.id = f.tbl_id`
  - `DELETE FROM tbl USING other f WHERE tbl.id = f.tbl_id`
  - 複数テーブルの結合条件および `RETURNING` 句との併用に完全対応。

### 実装できていない機能・制限事項 (Unsupported / Limitations)
- ※ 現在、標準 SQL および PostgreSQL 互換の主要 DML 機能はすべてサポートされています。

---

## 🔍 5. クエリ・データ検索 (DQL - Data Query Language)

### 実装できている機能 (Supported)
- **FROM 句なしの `SELECT` (SQL:2008 / モダン標準)**:
  - `SELECT 1 + 1 AS result, UPPER('hello') AS msg, NOW() AS cur_time;`
  - リテラル計算、関数呼び出し、`WHERE` 句を伴う純粋式評価。
- **共通テーブル式 (CTE: `WITH` 句 / SQL:1999)**:
  - `WITH cte AS (SELECT ...) SELECT ... FROM cte`
  - 単一 CTE、複数 CTE の順次定義（先行 CTE を参照するチェイン CTE）に対応。
  - メインクエリおよび JOIN 句（`JOIN cte ON ...`）での透過的な参照。
  - `INSERT INTO ... WITH ... SELECT ...` によるデータ転送クエリとの統合。
- **再帰共通テーブル式 (Recursive CTE / SQL:1999)**:
  - `WITH RECURSIVE cte AS (SELECT ... UNION ALL SELECT ... FROM cte ...) SELECT ...`
  - アンカークエリと再帰クエリの反復評価、階層データ（親子ツリー構造）や動的数列生成に対応。
  - 無限ループ防止のための最大再帰深度ガード（1,000回）内蔵。
- **列射影 (Projection)**:
  - 列名指定、別名付与 (`AS alias`)、ワイルドカード (`*`, `table.*`)、複合計算式。
- **フィルタリング (`WHERE`)**:
  - 比較演算子: `=`, `!=`, `<>`, `<`, `<=`, `>`, `>=` (異種数値型のクロス比較対応)
  - 論理演算子: `AND`, `OR`, `NOT` (括弧による優先順位制御対応)
  - パターンマッチング: `LIKE '%keyword%'`, `LIKE 'prefix%'`, `LIKE '%suffix'`
  - NULL 検証: `IS NULL`, `IS NOT NULL`
  - リスト包含判定: `IN (val1, val2, ...)` / `NOT IN (...)`
  - 範囲判定: `BETWEEN low AND high` / `NOT BETWEEN low AND high`
- **行値式の比較 (Row Value Constructors)**:
  - タプル等値・不等値比較: `WHERE (a, b) = (1, 2)`, `WHERE (a, b) != (1, 2)`
  - タプル順序・辞書順比較: `WHERE (a, b) > (1, 2)`, `WHERE (a, b) <= (10, 20)`
  - タプル IN リスト: `WHERE (a, b) IN ((1, 2), (3, 4))`
  - 複数列サブクエリ IN: `WHERE (a, b) IN (SELECT x, y FROM tbl)`
- **条件分岐式 (`CASE WHEN`)**:
  - `CASE WHEN cond1 THEN res1 WHEN cond2 THEN res2 ELSE default END`
  - 単純 CASE (`CASE expr WHEN val THEN ...`) および検索 CASE の双方に対応。
- **テーブル結合 (`JOIN`)**:
  - `INNER JOIN <table> ON <expr>`
  - `LEFT [OUTER] JOIN <table> ON <expr>`
  - `RIGHT [OUTER] JOIN <table> ON <expr>` (右外部結合)
  - `FULL [OUTER] JOIN <table> ON <expr>` (完全外部結合)
  - `CROSS JOIN <table>` (直積結合)
  - `NATURAL JOIN <table>` (同名共通列の自動等値結合)
  - `JOIN <table> USING (col1, col2, ...)` (指定列による簡潔結合構文)
  - 複数テーブルの連続結合（3テーブル以上の Chained Join）に対応。
  - テーブルエイリアス (`FROM users u JOIN orders o ON u.id = o.user_id`)。
- **集約とグループ化 (`GROUP BY`, `HAVING`)**:
  - 標準集約関数: `COUNT(*)`, `COUNT(col)`, `SUM(col)`, `AVG(col)`, `MIN(col)`, `MAX(col)`
  - 複数列による `GROUP BY`
  - グループ集約結果に対する `HAVING` フィルタリング
- **ソートとページネーション (`ORDER BY`, `LIMIT`, `OFFSET`)**:
  - 複数列ソート、昇順/降順 (`ASC` / `DESC`)
  - ページング: `LIMIT <n> OFFSET <m>`
- **重複排除 (`SELECT DISTINCT`)**:
  - 複数列プロジェクションに対する完全重複行の排除。
- **集合演算 (`UNION`, `INTERSECT`, `EXCEPT` / SQL-92)**:
  - `UNION` / `UNION ALL`: 和集合（重複排除 / 重複保持）
  - `INTERSECT` / `INTERSECT ALL`: 積集合（共通行の抽出）
  - `EXCEPT` / `EXCEPT ALL`: 差集合（左クエリから右クエリ行を除外）
  - 演算結果に対する `ORDER BY` および `LIMIT/OFFSET` の適用に対応。
- **ウィンドウ関数 (Window Functions / SQL:2003, SQL:2011)**:
  - `ROW_NUMBER() OVER ([PARTITION BY ...] [ORDER BY ...])`: 行番号付与 (1, 2, 3...)
  - `RANK() OVER ([PARTITION BY ...] [ORDER BY ...])`: 同点同位ランク・スキップあり (1, 1, 3...)
  - `DENSE_RANK() OVER ([PARTITION BY ...] [ORDER BY ...])`: 同点同位ランク・連番 (1, 1, 2...)
  - `LEAD(col [, offset [, default]]) OVER (...)`: 後続行の値参照
  - `LAG(col [, offset [, default]]) OVER (...)`: 先行行の値参照
  - `FIRST_VALUE(col) OVER (...)`: パーティション内最初の値
  - `LAST_VALUE(col) OVER (...)`: パーティション内最後の値
  - `NTILE(n) OVER (...)`: パーティション内のバケット等分割 (1..n)
  - 単一ソート、複数列 `PARTITION BY` / `ORDER BY`、式の中での複合利用（算術計算や CASE 式連携）に対応。
- **サブクエリ (Subqueries)**:
  - **派生テーブル (Derived Tables)**: `SELECT * FROM (SELECT ...) AS sub` (FROM 句および JOIN 句)
  - **IN サブクエリ**: `WHERE col IN (SELECT other_col FROM ...)`
  - **EXISTS サブクエリ**: `WHERE EXISTS (SELECT 1 FROM ... WHERE ...)`
  - **スカラサブクエリ**: `SELECT col, (SELECT MAX(...) FROM ...) AS max_val FROM tbl`
- **インデックス最適化 (`IndexScan`)**:
  - `WHERE col = <literal>` においてインデックスが存在する場合、オプティマイザが自動的に TableScan を回避して O(log N) の IndexScan を選択。
- **診断構文 (`EXPLAIN`)**:
  - `EXPLAIN SELECT ...` によるスキャン方式（`IndexScan` vs `TableScan`）、`NestedLoopJoin`、集約、ソートのツリー表示。

### 実装できていない機能・制限事項 (Unsupported / Limitations)
- ❌ **グルーピング拡張 (SQL:1999)**:
  - `GROUP BY ROLLUP(...)`, `CUBE(...)`, `GROUPING SETS(...)`

---

## 🔒 6. トランザクション制御 (TCL - Transaction Control)

### 実装できている機能 (Supported)
- **明示的トランザクション構文**:
  - `BEGIN;` / `START TRANSACTION;`
  - `COMMIT;` / `END;`
  - `ROLLBACK;`
- **分離レベル**:
  - **スナップショット分離 (Snapshot Isolation / MVCC)**
  - リーダーはライターをブロックせず、ライターもリーダーをブロックしない。
  - 同一キーに対する同時更新競合（Write-Write Conflict）の検知と安全なエラー拒絶。
- **デッドロック検出と自動キャンセル (Deadlock Detection & Victim Cancellation)**:
  - 待機グラフ（**Wait-For Graph**）をエンジン内部で維持し、閉路（Cycle）をリアルタイムに探索。
  - トランザクション間の相互ロック待機（デッドロック）を検知した場合、閉路を形成した要求元トランザクション（Victim）を即座に自動ロールバック（キャンセル）して `H2Error::LockConflict` を返却。
  - キャンセルされた側が保持していたロックが即座に解放されるため、待機中だった他トランザクションは直ちにブロック解除され、正常コミット可能。
  - ロック待機タイムアウト（デフォルト 1,000ms、設定可能）による無限待ち防止。
- **クエリ単位実行タイムアウト (Statement / Query Timeout)**:
  - クエリ個別指定（`query_timeout` / `execute_timeout` API）およびセッション全体でのデフォルトタイムアウト（`set_query_timeout`）をサポート。
  - PostgreSQL 互換の `SET statement_timeout = <ms>` および `SET query_timeout = <ms>` による動的設定に対応。
  - ストレージ層のロック待機（Wait-For）および B-Tree スキャンループと連動し、制限時間を超過したクエリを即座に安全中断（`H2Error::QueryTimeout`）。
- **Undo Log**:
  - トランザクション途中でエラーが発生した場合、または `ROLLBACK` 時に、Undo Log を逆順再生してコミット済み直前の状態へ完全復元。

### 実装できていない機能・制限事項 (Unsupported / Limitations)
- ⛔ **セーブポイント (`SAVEPOINT`) - 設計方針として実装対象外 (No Support by Design)**:
  - トランザクション内部での部分ロールバック（`SAVEPOINT s1; ... ROLLBACK TO SAVEPOINT s1;`）は、**MVCC スナップショット分離と Undo Log メカニズムの簡潔性・高速性を最優先とする本エンジンの設計方針に基づき、明確に実装対象外**としています。トランザクションエラー時は `ROLLBACK` による全体巻き戻しを行うか、アプリケーション層での小規模トランザクション分割をご利用ください。
- ❌ **分離レベルの動的変更**:
  - `SET TRANSACTION ISOLATION LEVEL READ COMMITTED / SERIALIZABLE` による分離レベル変更は未対応（スナップショット分離で統一）。
- ❌ **分散トランザクション (2PC / XA)**:
  - 外部コーディネータとの 2 相コミット（`PREPARE TRANSACTION`）は未対応。

---

## ⚡ 7. 組み込み関数・独自拡張 (Functions & Extensions)

### 実装できている機能 (Supported)
- **集約関数**: `COUNT`, `SUM`, `AVG`, `MIN`, `MAX`
- **制御関数**: `COALESCE(val1, val2, ...)`
- **文字列関数**:
  - `UPPER(str)`: 大文字変換
  - `LOWER(str)`: 小文字変換
  - `CONCAT(s1, s2, ...)`: 文字列連結
  - `LENGTH(str)`, `CHAR_LENGTH(str)`: 文字列の文字数カウント
- **数学関数**: `ABS(num)` (絶対値)
- **日時関数**: `NOW()`, `CURRENT_TIMESTAMP` (ISO 8601 / RFC 3339 形式の日時)
- **JSON 処理 (SQL:2016 / PG互換)**:
  - `->` (JSON オブジェクトの抽出・JSON型返却)
  - `->>` (JSON オブジェクトの抽出・文字列返却)
  - `JSON_EXTRACT(data, '$.path')`: JSONPath 式による階層アクセス
- **日本語全文検索エンジン (Core Native FTS / 独自)**:
  - `FT_SEARCH(col, 'キーワード')`: 漏れのない 2-Gram 転置インデックス走査
  - `FT_SEARCH_MORPH(col, '形態素')`: 文字種境界解析による高精度単語走査

### 実装できていない機能・制限事項 (Unsupported / Limitations)
- ❌ **高度な数学関数**: `POWER()`, `SQRT()`, `MOD()` (演算子 `%` で代替可能), 三角関数 (`SIN`, `COS`)
- ❌ **高度な文字列関数**: `SUBSTRING()`, `TRIM()`, `REPLACE()`, `LPAD()`, `RPAD()`
- ❌ **正規表現演算**: `~` (正規表現マッチ), `REGEXP_LIKE()`
- ❌ **日付計算関数**: `DATE_ADD()`, `DATE_SUB()`, `EXTRACT(YEAR FROM date)`

---

## 🚀 8. 今後のロードマップと拡張優先度

P1（高優先度）、P2（中優先度）、および P3 の `CREATE VIEW` までの機能群は**すべて実装完了**しました。

| 状態 | 対象機能 | 開発難易度 | 主な利用用途・効果 |
| :---: | :--- | :---: | :--- |
| ✅ **完了** | `INSERT INTO ... SELECT ...` | 中 | データ移行、サマリテーブル生成、バッチ処理 |
| ✅ **完了** | `WITH` 句 (非再帰 CTE) | 中 | 複雑な多段サブクエリの可読性・メンテナンス性向上 |
| ✅ **完了** | 外部キー制約 (`FOREIGN KEY`) | 高 | 親子テーブル間のリレーショナル整合性保証 (CASCADE/SET NULL/RESTRICT) |
| ✅ **完了** | ウィンドウ関数 (`ROW_NUMBER`, `RANK`, `DENSE_RANK`) | 高 | ランキング集計、ページ内ナンバリング、タイ順位計算 |
| ✅ **完了** | `INTERSECT`, `EXCEPT` (DISTINCT / ALL) | 低 | 集合演算の完全性（積集合・差集合） |
| ✅ **完了** | `TIMESTAMPTZ` (タイムゾーン保持型) | 中 | 国際化対応・タイムゾーン跨ぎの監査ログ |
| ✅ **完了** | `CREATE VIEW` / `DROP VIEW` (仮想ビュー) | 中 | クエリ共通化・結合ビュー・アクセス集約 |
| ⛔ **対象外** | `SAVEPOINT` (セーブポイント) | 中 | ※高速MVCCとUndo Log簡潔性維持のため設計上対象外 |
| **P3 (低)** | 再帰 CTE (`WITH RECURSIVE`) | 高 | 組織階層ツリーやグラフ構造の再帰探索 |
