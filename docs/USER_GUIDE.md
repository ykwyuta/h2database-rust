# H2 Database in Rust 利用者向け公式ガイド (User's Guide)

PostgreSQL の公式ドキュメント構成をベースに体系化した、`h2database-rust` の総合利用者向けガイドです。
SQLite のような手軽な組み込み利用から、PostgreSQL 互換サーバーとしての運用、高度な SQL 機能や日本語全文検索（FTS）、非同期 Rust アプリケーションへの統合までを網羅しています。

> [!NOTE]
> **SQL 標準規格との詳細な対比**:
> 国際標準（ISO/IEC 9075: SQL-92, SQL:1999, SQL:2003, SQL:2016 等）に対する「何が実装できていて、何が未実装・制限事項か」の完全な適合性マトリクスは [SQL 標準規格 適合状況と機能比較 (SQL_STANDARDS_COMPLIANCE.md)](./SQL_STANDARDS_COMPLIANCE.md) をご覧ください。

---

## 📚 目次 (Table of Contents)

- [Part I. チュートリアル (Tutorial)](#part-i-チュートリアル-tutorial)
  - [1. はじめの一歩 (Getting Started)](#1-はじめの一歩-getting-started)
  - [2. 同期 組み込みモード (Synchronous Embedded)](#2-同期-組み込みモード-synchronous-embedded)
  - [3. 非同期 組み込みモード (Async with Tokio)](#3-非同期-組み込みモード-async-with-tokio)
  - [4. PostgreSQL サーバーモード & 外部クライアント接続](#4-postgresql-サーバーモード--外部クライアント接続)
- [Part II. データ型 (Data Types)](#part-ii-データ型-data-types)
  - [1. 基本数値型・真偽値](#1-基本数値型真偽値)
  - [2. 文字列・バイナリ型](#2-文字列バイナリ型)
  - [3. 日付・時刻型 (Date/Time)](#3-日付時刻型-datetime)
  - [4. UUID & 高精度数値 (Decimal)](#4-uuid--高精度数値-decimal)
  - [5. JSON / JSONB 構造化データ](#5-json--jsonb-構造化データ)
- [Part III. SQL 言語ガイド (The SQL Language)](#part-iii-sql-言語ガイド-the-sql-language)
  - [1. テーブルとインデックスの定義 (DDL: 外部キー制約・CASCADE対応)](#1-テーブルとインデックスの定義-ddl-外部キー制約cascade対応)
  - [2. データ操作 (DML: INSERT, INSERT SELECT, UPDATE, DELETE)](#2-データ操作-dml-insert-insert-select-update-delete)
  - [3. クエリと検索 (SELECT, WHERE, ORDER BY, LIMIT/OFFSET)](#3-クエリと検索-select-where-order-by-limitoffset)
  - [4. テーブル結合 (JOIN: INNER, LEFT JOIN)](#4-テーブル結合-join-inner-left-join)
  - [5. 集約関数とグループ化 (GROUP BY, HAVING)](#5-集約関数とグループ化-group-by-having)
  - [6. 日本語対応 全文検索 (Full-Text Search: N-Gram & 形態素解析)](#6-日本語対応-全文検索-full-text-search-n-gram--形態素解析)
  - [7. メタデータ調査 (INFORMATION_SCHEMA)](#7-メタデータ調査-information_schema)
  - [8. テーブル構造の変更・全削除 (ALTER TABLE & TRUNCATE TABLE)](#8-テーブル構造の変更全削除-alter-table--truncate-table)
  - [9. 実行計画の確認とスキーマ照会 (EXPLAIN, SHOW TABLES, SHOW COLUMNS)](#9-実行計画の確認とスキーマ照会-explain-show-tables-show-columns)
  - [10. 高度なクエリ演算 (FROM句なしSELECT, CTE/WITH句, サブクエリ, DISTINCT, 集合演算 UNION/INTERSECT/EXCEPT, ウィンドウ関数)](#10-高度なクエリ演算-from句なしselect-ctewith句-サブクエリ-distinct-union)
  - [11. 仮想ビューの定義と管理 (CREATE VIEW, DROP VIEW)](#11-仮想ビューの定義と管理-create-view-drop-view)
- [Part IV. トランザクションと並行性制御 (Transactions & Concurrency)](#part-iv-トランザクションと並行性制御-transactions--concurrency)
  - [1. MVCC (マルチバージョン並行性制御) の特徴](#1-mvcc-マルチバージョン並行性制御-の特徴)
  - [2. Rust API によるトランザクション (RAII 管理)](#2-rust-api-によるトランザクション-raii-管理)
  - [3. SQL 文による明示的トランザクション (BEGIN, COMMIT, ROLLBACK)](#3-sql-文による明示的トランザクション-begin-commit-rollback)
  - [4. セーブポイント (SAVEPOINT) についての設計方針](#4-セーブポイント-savepoint-についての設計方針)
  - [5. デッドロック検出と自動キャンセル (Deadlock Detection & Victim Cancellation)](#5-デッドロック検出と自動キャンセル-deadlock-detection--victim-cancellation)
  - [6. クエリ単位の実行タイムアウト (Statement / Query Timeout)](#6-クエリ単位の実行タイムアウト-statement--query-timeout)

- [Part V. クライアント & 組み込み API ガイド (Client / Embedded APIs)](#part-v-クライアント--組み込み-api-ガイド-client--embedded-apis)
  - [1. パラメータ付きクエリと SQL インジェクション対策](#1-パラメータ付きクエリと-sql-インジェクション対策)
  - [2. 型安全なクエリ結果の走査 (`FromSql` & `Row::get_as`)](#2-型安全なクエリ結果の走査-fromsql--rowget_as)
  - [3. Web フレームワーク連携 (Axum / Actix-web での利用)](#3-web-フレームワーク連携-axum--actix-web-での利用)
- [Part VI. サーバー運用とメンテナンス (Server Administration & Maintenance)](#part-vi-サーバー運用とメンテナンス-server-administration--maintenance)
  - [1. 組み込みと外部接続のハイブリッド運用](#1-組み込みと外部接続のハイブリッド運用)
  - [2. DBeaver / DataGrip / psql からの接続](#2-dbeaver--datagrip--psql-からの接続)
  - [3. 対話型 CLI シェル (`h2-cli`) の使い方](#3-対話型-cli-シェル-h2-cli-の使い方)
  - [4. ストレージのコンパクション (Vacuum によるファイル縮小)](#4-ストレージのコンパクション-vacuum-によるファイル縮小)
- [Part VII. SQL コマンド & 関数リファレンス (Command Reference)](#part-vii-sql-コマンド--関数リファレンス-command-reference)

---

# Part I. チュートリアル (Tutorial)

## 1. はじめの一歩 (Getting Started)

Rust プロジェクトの `Cargo.toml` に `h2` の依存関係を追加します。

```toml
[dependencies]
h2 = { path = "path/to/h2database-rust/crates/h2" }
rust_decimal = "1.36"
uuid = "1.11"
chrono = "0.4"
serde_json = "1.0"
```

## 2. 同期 組み込みモード (Synchronous Embedded)

SQLite と同様に、プロセス内ライブラリとしてローカルファイルやインメモリで直接動作します。外部デーモンのインストールや設定は不要です。

```rust
use h2::{Connection, H2Result, params};
use rust_decimal::Decimal;
use std::str::FromStr;

fn main() -> H2Result<()> {
    // ファイル永続化モード（存在しない場合は自動生成）
    // インメモリの場合は Connection::open_in_memory()?
    let conn = Connection::open("app_data.h2")?;

    // 1. テーブルの作成 (DDL)
    conn.execute(
        "CREATE TABLE IF NOT EXISTS accounts (
            id INT PRIMARY KEY,
            owner VARCHAR(100) NOT NULL,
            balance DECIMAL(15, 2) NOT NULL
        )"
    )?;

    // 2. パラメータ付き INSERT
    conn.execute_params(
        "INSERT INTO accounts (id, owner, balance) VALUES (?, ?, ?)",
        &params![1, "Alice", Decimal::from_str("1500.50").unwrap()],
    )?;

    // 3. 型安全なクエリ走査
    let rows = conn.query("SELECT id, owner, balance FROM accounts WHERE balance > 1000")?;
    for row in rows {
        let id: i32 = row.get_as(0)?;
        let owner: String = row.get_as(1)?;
        let balance: Decimal = row.get_as(2)?;
        println!("Account #{id}: {owner} (Balance: ${balance})");
    }

    Ok(())
}
```

## 3. 非同期 組み込みモード (Async with Tokio)

Tokio 非同期ランタイムを用いたモダンな非同期アプリケーションや Web サービス向けに `AsyncConnection` を提供しています。内部で適切にスレッドプールへオフロードされるため、イベントループをブロックしません。

```rust
use h2::{AsyncConnection, H2Result, params};

#[tokio::main]
async fn main() -> H2Result<()> {
    let conn = AsyncConnection::open("async_app.h2").await?;

    conn.execute("CREATE TABLE IF NOT EXISTS logs (id INT PRIMARY KEY, message VARCHAR)").await?;
    conn.execute_params("INSERT INTO logs VALUES (?, ?)", &params![1, "Server started"]).await?;

    let rows = conn.query("SELECT message FROM logs WHERE id = 1").await?;
    if let Some(first) = rows.first() {
        let msg: String = first.get_as(0)?;
        println!("Log: {msg}");
    }

    Ok(())
}
```

## 4. PostgreSQL サーバーモード & 外部クライアント接続

本データベースは、組み込み動作を行いながらバックグラウンドで **PostgreSQL v3 ワイヤプロトコルサーバー（PG-Wire）** を起動できます。

```rust
use h2::{Connection, H2Result};
use std::net::SocketAddr;

#[tokio::main]
async fn main() -> H2Result<()> {
    let conn = Connection::open("shared.h2")?;

    // ポート 5432 (または 0 で空きポート自動選択) で PG サーバーを起動
    let addr: SocketAddr = "127.0.0.1:5432".parse().unwrap();
    let bound_addr = conn.start_pg_server(addr).await?;
    println!("PostgreSQL wire server is listening on {bound_addr}");

    // アプリケーション実行中、psql や DBeaver から同時に接続して中身を検査可能
    tokio::signal::ctrl_c().await.unwrap();
    Ok(())
}
```

---

# Part II. データ型 (Data Types)

本データベースは厳格な型付けを備えており、不正な型代入をコンパイルおよび実行時に防ぎます。

## 1. 基本数値型・真偽値

| SQL データ型 | Rust 対応型 | 範囲 / 仕様 |
| :--- | :--- | :--- |
| `BOOLEAN`, `BOOL` | `bool` | `TRUE`, `FALSE` |
| `TINYINT` | `i8` | -128 〜 127 |
| `SMALLINT`, `INT2` | `i16` | -32,768 〜 32,767 |
| `INTEGER`, `INT`, `INT4` | `i32` | -2,147,483,648 〜 2,147,483,647 |
| `BIGINT`, `INT8` | `i64` | -9,223,372,036,854,775,808 〜 9,223,372,036,854,775,807 |
| `REAL`, `FLOAT` | `f32` | 単精度浮動小数点数 |
| `DOUBLE`, `DOUBLE PRECISION` | `f64` | 倍精度浮動小数点数 |

## 2. 文字列・バイナリ型

| SQL データ型 | Rust 対応型 | 仕様 |
| :--- | :--- | :--- |
| `VARCHAR(n)`, `VARCHAR` | `String` | 可変長 UTF-8 文字列（`n` 指定時は長さ検証） |
| `CHAR(n)`, `CHAR` | `String` | 固定長 / 文字列 |
| `TEXT` | `String` | 無制限の長文テキスト（全文検索対象に推奨） |
| `BINARY`, `BYTEA`, `BLOB`| `Vec<u8>` | 任意のバイナリバイト列 |

## 3. 日付・時刻型 (Date/Time & TIMESTAMPTZ)

| SQL データ型 | Rust 対応型 (`chrono`) | 例 / フォーマット |
| :--- | :--- | :--- |
| `DATE` | `chrono::NaiveDate` | `'2026-09-24'` |
| `TIME` | `chrono::NaiveTime` | `'20:30:00'` |
| `TIMESTAMP` | `chrono::DateTime<Utc>` / `NaiveDateTime` | `'2026-09-24 20:30:00'` |
| `TIMESTAMPTZ`, `TIMESTAMP WITH TIME ZONE` | `chrono::DateTime<Utc>` | `'2026-09-24T12:00:00+09:00'`, `'2026-09-24 03:00:00Z'` |

> [!TIP]
> **タイムゾーンの自動正規化とクロス比較**:
> `TIMESTAMPTZ` 型の列に格納される値は、指定されたタイムゾーンオフセット（例: `+09:00`, `-05:00`, `Z` 等）を正確に解釈し、内部で協定世界時（UTC）へ正規化されます。
> そのため、`'2026-09-24T12:00:00+09:00'` と `'2026-09-24 03:00:00Z'` は等値（`=`）として判定され、タイムゾーンを跨ぐ監査ログやグローバルなイベント時刻比較を安全に行えます。

```sql
CREATE TABLE audit_events (
    id INT PRIMARY KEY,
    event_name VARCHAR NOT NULL,
    occurred_at TIMESTAMPTZ NOT NULL
);

-- JST (+09:00) の正午を挿入
INSERT INTO audit_events VALUES (1, 'UserLogin', '2026-09-24 12:00:00+09:00');

-- UTC (Z) 03:00 として検索（同じ絶対時刻のため一致する）
SELECT * FROM audit_events WHERE occurred_at = '2026-09-24 03:00:00Z';
```

## 4. UUID & 高精度数値 (Decimal)

金融・決済処理など、丸め誤差が許されない計算には `DECIMAL` を、分散一意識別子には `UUID` をネイティブ利用できます。

```sql
CREATE TABLE orders (
    order_id UUID PRIMARY KEY,
    amount DECIMAL(15, 2) NOT NULL,
    discount DECIMAL(5, 4) DEFAULT 0.0000
);
```

```rust
use uuid::Uuid;
use rust_decimal::Decimal;
use std::str::FromStr;

let order_id = Uuid::new_v4();
let amount = Decimal::from_str("1299.95").unwrap();
conn.execute_params(
    "INSERT INTO orders VALUES (?, ?, ?)",
    &[Value::Uuid(order_id), Value::Decimal(amount), Value::Decimal(Decimal::ZERO)],
)?;
```

## 5. JSON / JSONB 構造化データ

PostgreSQL のように JSON データをテーブルの列に保存し、JSON 演算子や関数で内部プロパティを直接検索できます。

```sql
CREATE TABLE user_profiles (
    id INT PRIMARY KEY,
    attributes JSON
);

INSERT INTO user_profiles VALUES (1, '{"role": "admin", "settings": {"theme": "dark", "notifications": true}}');
```

### JSON 演算子と抽出関数:
- `attributes -> 'key'`: JSON 型のままプロパティを抽出
- `attributes ->> 'key'`: テキスト（文字列）としてプロパティを抽出
- `JSON_EXTRACT(col, '$.path.to.prop')`: ドットパスによる階層抽出

```sql
-- テーマが dark のユーザーを検索
SELECT id, attributes -> 'settings' ->> 'theme' AS theme
FROM user_profiles
WHERE attributes -> 'settings' ->> 'theme' = 'dark';
```

---

# Part III. SQL 言語ガイド (The SQL Language)

## 1. テーブルとインデックスの定義 (DDL: 外部キー制約・CASCADE対応)

### テーブルの作成と削除
```sql
-- 親テーブルの作成
CREATE TABLE IF NOT EXISTS departments (
    id INT PRIMARY KEY,
    name VARCHAR(50) NOT NULL
);

-- テーブルの削除 (IF EXISTS 対応)
DROP TABLE IF EXISTS old_users;
```

### 外部キー制約 (FOREIGN KEY & 参照整合性)
親子テーブル間の参照整合性を強制し、親行の削除・更新に伴う連動アクション（`CASCADE`、`SET NULL`、`RESTRICT`）をサポートしています。

```sql
-- ① カラムレベルの参照制約 (デフォルト RESTRICT)
CREATE TABLE employees (
    id INT PRIMARY KEY,
    name VARCHAR(50) NOT NULL,
    dept_id INT REFERENCES departments(id)
);

-- ② テーブル制約レベル & 連動削除 (ON DELETE CASCADE)
CREATE TABLE projects (
    id INT PRIMARY KEY,
    title VARCHAR(100) NOT NULL,
    dept_id INT,
    FOREIGN KEY (dept_id) REFERENCES departments(id) ON DELETE CASCADE
);

-- ③ 連動 NULL 化 (ON DELETE SET NULL) および連動更新 (ON UPDATE CASCADE)
CREATE TABLE audit_logs (
    id INT PRIMARY KEY,
    action VARCHAR(100) NOT NULL,
    dept_id INT,
    FOREIGN KEY (dept_id) REFERENCES departments(id) 
        ON DELETE SET NULL 
        ON UPDATE CASCADE
);
```

- **整合性バリデーション**: 子テーブルへ存在しない `dept_id` を INSERT / UPDATE しようとすると制約エラーとなります（`NULL` 値は SQL 標準通り許容）。
- **RESTRICT / NO ACTION**: 子テーブルから参照されている親行の DELETE や更新は自動的に拒絶されます。
- **CASCADE**: 親行を DELETE すると参照する子行も自動削除され、親キーを UPDATE すると子行の外部キー列も自動更新されます。
- **SET NULL**: 親行を DELETE / UPDATE すると、参照する子行の該当列が自動的に `NULL` に更新されます。
- **被参照テーブル保護**: 子行が存在する状態で親テーブルを `DROP TABLE` または `TRUNCATE TABLE` しようとすると拒絶されます。

### インデックスの作成と削除
本データベースは追記型 B-Tree 上にセカンダリインデックスおよび一意（UNIQUE）インデックスを作成できます。

```sql
-- 通常のセカンダリインデックス
CREATE INDEX idx_users_age ON users (age);

-- 重複を禁止する一意インデックス
CREATE UNIQUE INDEX idx_users_email ON users (email);

-- インデックスの削除
DROP INDEX IF EXISTS idx_users_age;
```

> [!TIP]
> **自動 IndexScan 最適化**:
> `WHERE email = 'alice@example.com'` のような検索を実行すると、オプティマイザが自動的にインデックスの存在を検知し、テーブル全件走査（SeqScan）をスキップしてインデックス走査（IndexScan）を行い、O(log N) で高速取得します。

## 2. データ操作 (DML: INSERT, INSERT SELECT, UPDATE, DELETE)

### INSERT (単行・複数行挿入)
```sql
INSERT INTO users (id, email, age) VALUES (1, 'alice@example.com', 25);
INSERT INTO users VALUES (2, 'bob@example.com', 30, CURRENT_TIMESTAMP);
```

### INSERT INTO ... SELECT ... (クエリ結果の直接挿入)
別テーブルからのデータ移行、集約集計テーブルの自動生成、CTE（共通テーブル式）からの直接挿入に対応しています。

```sql
-- 全カラム直接転送
INSERT INTO user_archive SELECT * FROM users WHERE age >= 60;

-- 特定カラム指定 & 式計算の転送
INSERT INTO user_bonuses (user_id, bonus_amount) 
SELECT id, age * 100 FROM users WHERE age >= 20;

-- GROUP BY 集約結果をサマリテーブルに保存
INSERT INTO department_stats (dept_id, employee_count, avg_salary)
SELECT dept_id, COUNT(*), AVG(salary) FROM employees GROUP BY dept_id;

-- CTE (WITH 句) と組み合わせた挿入
INSERT INTO vip_customers
WITH high_spenders AS (
    SELECT customer_id, SUM(amount) AS total 
    FROM orders 
    GROUP BY customer_id 
    HAVING SUM(amount) >= 100000
)
SELECT customer_id, total FROM high_spenders;
```

### UPDATE
自己参照式（`age = age + 1`）や複数カラムの更新に対応しています。外部キー整合性も自動検証されます。
```sql
UPDATE users 
SET age = age + 1, email = 'alice_new@example.com' 
WHERE id = 1;
```

### DELETE
親行削除時の連動アクション（CASCADE / SET NULL）および参照保護（RESTRICT）が機能します。
```sql
DELETE FROM users WHERE age < 18;
```

## 3. クエリと検索 (SELECT, WHERE, ORDER BY, LIMIT/OFFSET)

```sql
SELECT id, email, age
FROM users
WHERE age >= 20 AND email LIKE '%@example.com'
ORDER BY age DESC, id ASC
LIMIT 10 OFFSET 20;
```

## 4. テーブル結合 (JOIN: INNER, LEFT JOIN)

複数テーブルの結合、テーブル修飾カラム名（`u.id`）、テーブルエイリアスを完全サポートしています。

```sql
-- ユーザーと注文履歴の内部結合 (INNER JOIN)
SELECT u.name, o.item_name, o.price
FROM users u
INNER JOIN orders o ON u.id = o.user_id
WHERE o.price > 50.00;

-- 注文のないユーザーも含む外部結合 (LEFT OUTER JOIN)
SELECT u.name, o.item_name
FROM users u
LEFT JOIN orders o ON u.id = o.user_id;
```

## 5. 集約関数とグループ化 (GROUP BY, HAVING)

```sql
SELECT 
    department,
    COUNT(*) AS total_employees,
    SUM(salary) AS total_salary,
    AVG(salary) AS avg_salary,
    MIN(salary) AS min_salary,
    MAX(salary) AS max_salary
FROM employees
WHERE is_active = TRUE
GROUP BY department
HAVING AVG(salary) >= 50000
ORDER BY avg_salary DESC;
```

## 6. 日本語対応 全文検索 (Full-Text Search: N-Gram & 形態素解析)

SQLite や通常の RDBMS では外部拡張が必要となる日本語全文検索を、コアエンジンにネイティブ統合しています。

### ① N-Gram トークナイザ (`FT_SEARCH`)
文字列を 2 文字ずつ（Bigram）スライディングウィンドウで分解して転置インデックスを構成します。日本語・英語・数字・記号を問わず検索漏れ（再現率 100%）がありません。

```sql
SELECT id, title, content
FROM articles
WHERE FT_SEARCH(content, 'データベース');
```

### ② 形態素・単語境界トークナイザ (`FT_SEARCH_MORPH`)
漢字・ひらがな・カタカナ・アルファベットの文字種境界を解析し、日本語テキストを意味のあるトークンに分割して検索します。不要なノイズの少ない高精度な検索が可能です。

```sql
SELECT id, title
FROM manual
WHERE FT_SEARCH_MORPH(title, '設計 ドキュメント');
```

## 7. メタデータ調査 (INFORMATION_SCHEMA)

PostgreSQL 標準の `INFORMATION_SCHEMA` システムビューを提供しているため、DBeaver やプログラムからスキーマ定義を容易に探索できます。

```sql
-- 存在するすべてのテーブル一覧
SELECT table_name FROM information_schema.tables;

-- 特定テーブルのカラム構造とデータ型定義
SELECT column_name, data_type, is_nullable, is_primary_key
FROM information_schema.columns
WHERE table_name = 'users'
ORDER BY ordinal_position;
```

## 8. テーブル構造の変更・全削除 (ALTER TABLE & TRUNCATE TABLE)

稼働中のテーブルに対するスキーマ変更（列の追加・削除、テーブル名変更）や、大量データの高速な一括削除に対応しています。

### ① テーブル名の変更 (`ALTER TABLE ... RENAME TO`)
データおよび関連インデックスを保持したままテーブル名を変更します。

```sql
ALTER TABLE customers RENAME TO clients;
```

### ② カラムの追加 (`ALTER TABLE ... ADD COLUMN`)
既存テーブルに新しい列を追加します。既存レコードには自動的に `NULL` が補完されます。

```sql
ALTER TABLE employees ADD COLUMN department VARCHAR(50);
```

### ③ カラムの削除 (`ALTER TABLE ... DROP COLUMN`)
テーブル定義および既存レコードから対象列のデータを安全に削除します。

```sql
ALTER TABLE users DROP COLUMN temp_token;
```

### ④ テーブルデータの全削除 (`TRUNCATE TABLE`)
テーブルの定義およびスキーマ構造を維持したまま、格納されている全レコードおよび関連インデックスを即座に破棄し、自動採番カウンタ（Row ID）を初期化します。

```sql
TRUNCATE TABLE logs;
```

## 9. 実行計画の確認とスキーマ照会 (EXPLAIN, SHOW TABLES, SHOW COLUMNS)

データベースのパフォーマンスチューニングや対話型シェルでの探索を支援する診断構文をサポートしています。

### ① 実行計画の表示 (`EXPLAIN`)
SQL クエリがどのようにスキャン（`IndexScan` vs `TableScan`）、結合（`NestedLoopJoin`）、集約、ソートされるかをテキスト形式で出力します。

```sql
EXPLAIN SELECT * FROM users WHERE email = 'test@example.com';
```

**出力例**:
```text
+----------------------------------------------------+
| PLAN                                               |
+----------------------------------------------------+
| IndexScan: users on index idx_users_email          |
| Filter: email = 'test@example.com'                 |
| Projection: *                                      |
+----------------------------------------------------+
```

### ② テーブル一覧の照会 (`SHOW TABLES`)
現在定義されているテーブル名を一覧表示します。

```sql
SHOW TABLES;
```

### ③ テーブルカラム定義の照会 (`SHOW COLUMNS FROM <table>`)
指定したテーブルのカラム一覧、データ型、NULL 許可、主キー情報を取得します。

```sql
SHOW COLUMNS FROM users;
```

## 10. 高度なクエリ演算 (FROM句なしSELECT, CTE/WITH句, サブクエリ, DISTINCT, UNION)

複雑なデータ抽出や計算処理に対応するモダンな SQL 構文を豊富にサポートしています。

### ① FROM 句なしの計算・式評価 (FROM-less SELECT)
テーブルを参照せず、リテラル計算や関数評価、条件分岐を即座に評価できます。

```sql
SELECT 1 + 1 AS result, UPPER('hello') AS greeting, NOW() AS current_time;
```

### ② 共通テーブル式 (CTE: `WITH` 句 / SQL:1999)
複雑な多段サブクエリをモジュール化し、可読性の高いクエリ構造を作成できます。単一 CTE、先行 CTE を参照するチェイン CTE、テーブルとの JOIN、`INSERT INTO ... SELECT` との組み合わせに対応しています。

```sql
-- 1. 単一 CTE
WITH high_scorers AS (
    SELECT id, name, score 
    FROM students 
    WHERE score >= 80
)
SELECT name, score FROM high_scorers ORDER BY score DESC;

-- 2. 複数 CTE の順次定義 (チェイン CTE)
WITH regional_sales AS (
    SELECT region, SUM(amount) AS total_sales
    FROM orders
    GROUP BY region
),
top_regions AS (
    SELECT region, total_sales
    FROM regional_sales
    WHERE total_sales > 1000000
)
SELECT * FROM top_regions ORDER BY total_sales DESC;

-- 3. CTE と実テーブルの JOIN
WITH active_users AS (
    SELECT id, name FROM users WHERE is_active = TRUE
)
SELECT u.name, o.item_name, o.amount
FROM active_users u
JOIN orders o ON u.id = o.user_id
ORDER BY o.amount DESC;
```

### ③ 条件分岐式 (`CASE WHEN ... THEN ... ELSE ... END`)
行ごとの動的な値変換やラベル付けが可能です。

```sql
SELECT name,
       CASE 
           WHEN score >= 90 THEN 'A'
           WHEN score >= 70 THEN 'B'
           ELSE 'C'
       END AS rank
FROM students;
```

### ④ `IN` および `BETWEEN` 式
複数の候補値や範囲指定による簡潔な絞り込みに対応しています。

```sql
SELECT * FROM products WHERE category_id IN (1, 3, 5);
SELECT * FROM orders WHERE amount BETWEEN 100 AND 500;
```

### ⑤ 派生テーブル (FROM 句 / JOIN 句のサブクエリ)
集約クエリや複雑な中間結果をインメモリの派生テーブル（Derived Table）として扱い、さらにフィルタや結合を行えます。

```sql
-- 集約結果をサブクエリとして再フィルタ
SELECT sub.user_id, sub.total_amount
FROM (
    SELECT user_id, SUM(amount) AS total_amount
    FROM orders
    GROUP BY user_id
) AS sub
WHERE sub.total_amount >= 10000;

-- サブクエリとの JOIN
SELECT u.name, sub.total_amount
FROM users u
JOIN (
    SELECT user_id, SUM(amount) AS total_amount
    FROM orders
    GROUP BY user_id
) AS sub ON u.id = sub.user_id;
```

### ⑥ `IN` / `EXISTS` サブクエリ & スカラサブクエリ
WHERE 句での動的条件判定や、列定義でのスカラサブクエリを利用できます。

```sql
-- IN サブクエリ
SELECT name FROM users WHERE id IN (SELECT user_id FROM orders WHERE amount >= 500);

-- EXISTS サブクエリ
SELECT name FROM users WHERE EXISTS (SELECT 1 FROM orders WHERE orders.user_id = users.id);

-- スカラサブクエリ
SELECT name, (SELECT MAX(amount) FROM orders) AS max_order FROM users;
```

### ⑦ 重複排除 (`SELECT DISTINCT`)
抽出結果から完全に重複する行を除去します。

```sql
SELECT DISTINCT department FROM employees ORDER BY department;
```

### ⑧ 集合演算 (`UNION`, `INTERSECT`, `EXCEPT` / SQL-92)
複数のクエリ結果を集合演算として統合・抽出します。デフォルトは重複排除（DISTINCT）、`ALL` 指定時は重複保持となります。

- **`UNION` / `UNION ALL` (和集合)**: 両クエリの行を統合。
- **`INTERSECT` / `INTERSECT ALL` (積集合)**: 両クエリの双方に存在する共通行のみを抽出。
- **`EXCEPT` / `EXCEPT ALL` (差集合)**: 左クエリの結果から右クエリの結果を除外。

```sql
-- 1. 和集合 (重複排除)
SELECT name, email FROM internal_users
UNION
SELECT name, email FROM external_partners
ORDER BY name;

-- 2. 積集合 (両方の部署に所属しているユーザー)
SELECT user_id FROM sales_members
INTERSECT
SELECT user_id FROM dev_members
ORDER BY user_id;

-- 3. 差集合 (注文履歴のないユーザー)
SELECT id FROM users
EXCEPT
SELECT user_id FROM orders
ORDER BY id;
```

### ⑨ ウィンドウ関数 (Window Functions: `ROW_NUMBER`, `RANK`, `DENSE_RANK` / SQL:2003)
行をグループに縮約（集約）することなく、現在行に関連する行セット（ウィンドウ）に基づいて順位や連番を計算します。
`OVER` 句内で `PARTITION BY`（グループ分割）および `ORDER BY`（順序付け）を指定できます。

- **`ROW_NUMBER()`**: 各行に 1 から始まる一意の連番を割り当てます。
- **`RANK()`**: 値が同一の場合は同じ順位を割り当て、タイの個数分だけ後続の順位をスキップします（例: 1, 2, 2, 4...）。
- **`DENSE_RANK()`**: 値が同一の場合は同じ順位を割り当てますが、後続の順位をスキップせず連番を保ちます（例: 1, 2, 2, 3...）。

```sql
-- 部門ごとの給与ランキングクエリ
SELECT 
    id, 
    dept, 
    salary,
    ROW_NUMBER() OVER (PARTITION BY dept ORDER BY salary DESC) AS row_num,
    RANK() OVER (PARTITION BY dept ORDER BY salary DESC) AS rank,
    DENSE_RANK() OVER (PARTITION BY dept ORDER BY salary DESC) AS dense_rank
FROM employees
ORDER BY dept, row_num;

-- 全体ソートでの連番ナンバリング
SELECT id, title, ROW_NUMBER() OVER (ORDER BY created_at DESC) AS seq_no
FROM articles;
```

## 11. 仮想ビューの定義と管理 (CREATE VIEW, DROP VIEW)

複雑な結合や集約クエリを名前付きの「仮想テーブル」として保存し、通常のテーブルと同様に SELECT や JOIN の対象として透過的に再利用できます。
ビュー定義はカタログに永続化され、クエリ実行時に自動的に動的展開されます。

### ① ビューの作成 (`CREATE VIEW` / `CREATE OR REPLACE VIEW`)
```sql
-- 1. 基本ビューの作成
CREATE VIEW active_customers AS
SELECT id, name, email
FROM users
WHERE is_active = TRUE AND role = 'customer';

-- 2. 列名を明示的に指定したビュー
CREATE VIEW monthly_sales_summary (month_str, total_amount) AS
SELECT order_month, SUM(amount)
FROM orders
GROUP BY order_month;

-- 3. 既存ビューの置換 (CREATE OR REPLACE VIEW)
CREATE OR REPLACE VIEW active_customers AS
SELECT id, name, email, updated_at
FROM users
WHERE is_active = TRUE;
```

### ② ビューの利用 (SELECT & テーブル結合)
ビューは実テーブルと全く同様に、`WHERE` フィルタ、テーブルとの `JOIN`、CTE との併用が可能です。

```sql
-- ビューからのデータ抽出
SELECT name, email FROM active_customers WHERE name LIKE 'A%' ORDER BY name;

-- ビューと実テーブルの結合 (JOIN)
SELECT c.name, o.amount, o.ordered_at
FROM active_customers c
JOIN orders o ON c.id = o.user_id
WHERE o.amount >= 10000
ORDER BY o.amount DESC;
```

### ③ ビューの削除 (`DROP VIEW`)
```sql
DROP VIEW active_customers;

-- 存在しない場合のエラー抑止
DROP VIEW IF EXISTS active_customers;
```

---

# Part IV. トランザクションと並行性制御 (Transactions & Concurrency)

## 1. MVCC (マルチバージョン並行性制御) の特徴

本データベースは PostgreSQL や H2 Database と同様の **追記型スナップショット分離 (Snapshot Isolation)** を採用しています。
- **リーダーはライターをブロックしない**: 巨大な集計クエリやバックアップ走査を実行中でも、同時に INSERT/UPDATE を高速実行可能。
- **ライターはリーダーをブロックしない**: 書き込み中であっても、リーダーは開始時点の過去コミットスナップショットを一貫して読み取り可能。
- **ファースト・コッター勝者ルール**: 同一レコードに対する同時更新（Write-Write 競合）は検知され、トランザクションエラーとして安全に拒絶されます。

## 2. Rust API によるトランザクション (RAII 管理)

```rust
let conn = Connection::open("bank.h2")?;

// トランザクションを開始 (スナップショット確定)
let tx = conn.transaction()?;

tx.execute_params("UPDATE accounts SET balance = balance - 100 WHERE id = ?", &params![1])?;
tx.execute_params("UPDATE accounts SET balance = balance + 100 WHERE id = ?", &params![2])?;

// コミットして永続化確定
tx.commit()?;

// ※ tx が commit() されずにスコープを抜けた場合、Drop 実装により自動的に安全にロールバックされます。
```

## 3. SQL 文による明示的トランザクション (BEGIN, COMMIT, ROLLBACK)

PostgreSQL クライアントやスクリプトから、テキストの SQL コマンドでセッション単位のトランザクションを直接制御できます。

```sql
BEGIN;
INSERT INTO inventory (item, qty) VALUES ('Monitor', 10);
-- 何らかのエラー発生時
ROLLBACK;

BEGIN TRANSACTION;
UPDATE inventory SET qty = qty - 1 WHERE item = 'Keyboard';
COMMIT;
```

## 4. セーブポイント (SAVEPOINT) についての設計方針

> [!IMPORTANT]
> **SAVEPOINT は設計方針として実装対象外（No Support by Design）です**:
> 本エンジンは、追記型 B-Tree（MVStore）による**高速な MVCC スナップショット分離**と、トランザクション単位の**逆順 Undo Log 再生**による簡潔かつ堅牢な整合性復元をコアアーキテクチャとしています。
> トランザクション途中での部分巻き戻し（`SAVEPOINT name` / `ROLLBACK TO SAVEPOINT name`）を導入することは、内部 Undo ログ構造の過度な複雑化と実行時オーバーヘッドを招くため、設計上サポート対象外（明確なスコープ外）としています。
> 
> - **推奨されるベストプラクティス**:
>   - エラー発生時は `ROLLBACK` を発行してトランザクション全体を安全に破棄する。
>   - 大量データ処理において部分的な失敗を許容したい場合は、一連の処理を複数の小さなトランザクションに分割して逐次コミットする。

## 5. デッドロック検出と自動キャンセル (Deadlock Detection & Victim Cancellation)


複数のトランザクションが互いに相手の保持するレコードの排他ロックを待ち合う**デッドロック（循環依存）**が発生した場合、エンジンはこれを自動検出し、片方のトランザクションを安全にキャンセル（自動ロールバック）します。

### ① デッドロックの発生と自動解決のフロー
1. **待機グラフ（Wait-For Graph）の監視**:
   - トランザクション $T_A$ がロック中のキーにトランザクション $T_B$ がアクセスした際、エンジンは待機関係 $T_B \to T_A$ を記録し、ロック解放を待機します。
2. **閉路（サイクル）の即時検出**:
   - $T_A$ が $T_B$ の保持する別のキーを要求した場合、待機グラフ上にサイクル（$T_A \to T_B \to T_A$）が形成されます。
   - エンジンは DFS による閉路検出アルゴリズムにより、このデッドロックを瞬時に検知します。
3. **被害者（Victim）トランザクションの自動キャンセル**:
   - デッドロックを形成した要求元トランザクションを自動的にロールバック（Undo Log を適用して変更を破棄・ロック全解放）します。
   - キャンセルされた側には `H2Error::LockConflict`（`Deadlock detected: transaction ... cancelled to break cycle`）が返却されます。
4. **もう片方のトランザクションのブロック解除と続行**:
   - キャンセルされた側が保持していたロックが即座に解放されるため、もう片方のトランザクションは直ちに待機から復帰し、正常にコミットまで完了できます。

```sql
-- セッション 1 (Tx1)                    -- セッション 2 (Tx2)
BEGIN;                                  BEGIN;
UPDATE accounts SET bal=100 WHERE id=1; 
                                        UPDATE accounts SET bal=200 WHERE id=2;
-- id=2 のロック解放待ち (待機開始)
UPDATE accounts SET bal=150 WHERE id=2;
                                        -- id=1 を要求 -> デッドロック即時検出！
                                        UPDATE accounts SET bal=250 WHERE id=1;
                                        -- => ERROR: Deadlock detected (Tx2 自動ロールバック)
-- Tx2 のロールバックによりブロック解除
-- Tx1 は正常に完了
COMMIT;
```

### ② ロック待機タイムアウトの設定
デッドロック以外の理由で長時間ロックが解放されない事態を防ぐため、ロック待機タイムアウト機能（デフォルト: 1,000ms）を備えています。
組み込み Rust API では接続単位・エンジン単位でタイムアウト時間をカスタマイズできます。

```rust
// ロック待機タイムアウトを 3,000ミリ秒 (3秒) に変更
conn.set_lock_timeout_ms(3000);
```

### ③ アプリケーション側での推奨対処パターン
デッドロックエラー（`LockConflict`）を受信した場合、アプリケーション側では以下のように指数バックオフ（Exponential Backoff）を伴うリトライを実装することが推奨されます。

```rust
let mut retries = 3;
while retries > 0 {
    let res = run_business_transaction(&conn);
    match res {
        Ok(_) => break,
        Err(H2Error::LockConflict(msg)) if msg.contains("Deadlock detected") => {
            // トランザクションは既に自動ロールバック済み。セッション状態を整えてリトライ
            let _ = conn.execute("ROLLBACK");
            std::thread::sleep(std::time::Duration::from_millis(50));
            retries -= 1;
        }
        Err(e) => return Err(e),
    }
}
```

## 6. クエリ単位の実行タイムアウト (Statement / Query Timeout)

長時間実行される重いクエリや、他トランザクションの行ロック待ちによって処理が長時間ブロックされることを防ぐため、**クエリ単位の実行タイムアウト**をサポートしています。

タイムアウトに達した場合、エンジンは処理を即座に中断し、`H2Error::QueryTimeout("Query execution timed out")` を返却します。

### ① クエリ単位での明示的タイムアウト指定
`query_timeout` / `execute_timeout` API を使用し、個別の SQL 実行ごとに制限時間をミリ秒・秒単位で指定できます。

```rust
use std::time::Duration;

// 1. SELECT クエリに 500ミリ秒のタイムアウトを指定
let rows = conn.query_timeout(
    "SELECT * FROM large_table WHERE status = 'pending'",
    Duration::from_millis(500),
)?;

// 2. パラメータ付きクエリでの指定
let rows = conn.query_params_timeout(
    "SELECT * FROM orders WHERE amount > ?1",
    &params![10000],
    Duration::from_secs(1),
)?;

// 3. DML / DDL 更新文での指定
let affected = conn.execute_timeout(
    "UPDATE accounts SET balance = balance + 10 WHERE id = 1",
    Duration::from_millis(200),
)?;
```

### ② セッション全体のデフォルトタイムアウト設定
セッション（`Connection`）単位でデフォルトのタイムアウト時間を設定しておくと、通常の `query()` や `execute()` の実行時にも自動的にタイムアウトが適用されます。

```rust
// セッションのデフォルトクエリタイムアウトを 1,000ms (1秒) に設定
conn.set_query_timeout_ms(1000);

// 自動的に 1秒のタイムアウトが適用される
let rows = conn.query("SELECT * FROM complex_view")?;

// タイムアウトを解除（無期限化）
conn.set_query_timeout(None);
```

### ③ PostgreSQL 互換 SQL 文によるタイムアウト設定
`psql`、JDBC、外部アプリケーションから接続している場合、標準的な `SET statement_timeout` または `SET query_timeout` コマンドで動的に設定・解除できます。

```sql
-- 現在のセッションのクエリタイムアウトを 500ミリ秒に設定
SET statement_timeout = 500;

-- 重い集約クエリ (500ms を超過すると自動的に ERROR: Query timeout が返る)
SELECT dept, AVG(salary) FROM employees GROUP BY dept;

-- タイムアウトを解除 (0 = 無制限)
SET statement_timeout = 0;
```

### ④ 非同期 Rust API (`AsyncConnection`) での利用
Tokio ネイティブの非同期環境でも、同様にクエリ単位のタイムアウトが利用可能です。

```rust
use std::time::Duration;

// 非同期クエリタイムアウト
let rows = async_conn.query_timeout(
    "SELECT * FROM items ORDER BY created_at DESC LIMIT 50",
    Duration::from_millis(300),
).await?;
```

---

# Part V. クライアント & 組み込み API ガイド (Client / Embedded APIs)

## 1. パラメータ付きクエリと SQL インジェクション対策

`?`、`?1`、`$1` などのプレースホルダーを使用し、引数を安全にバインドします。

```rust
use h2::{params, Value};

// params! マクロで直感的にパラメータリストを作成
conn.execute_params(
    "INSERT INTO users (name, age, note) VALUES ($1, $2, $3)",
    &params!["Charlie", 28, "Engineer"],
)?;

// 手動で Value を指定することも可能
conn.query_params(
    "SELECT * FROM users WHERE age > ?1 AND name = ?2",
    &[Value::Integer(20), Value::String("Charlie".to_string())],
)?;
```

## 2. 型安全なクエリ結果の走査 (`FromSql` & `Row::get_as`)

`Row::get_as::<T>(index)` により、型安全な変換・抽出が可能です。

```rust
let rows = conn.query("SELECT id, name, age, optional_bio FROM users")?;
for row in rows {
    let id: i32 = row.get_as(0)?;
    let name: String = row.get_as(1)?;
    let age: i32 = row.get_as(2)?;
    
    // NULL 許容カラムは Option<T> で安全に取得
    let bio: Option<String> = row.get_as(3)?;
    
    println!("User #{id}: {name}, Age: {age}, Bio: {bio:?}");
}
```

## 3. Web フレームワーク連携 (Axum / Actix-web での利用)

`AsyncConnection` はスレッド安全（`Send + Sync + Clone`）であるため、Web アプリの共有状態（`State`）にそのまま登録して各リクエストハンドラから呼び出せます。

```rust
use axum::{extract::State, routing::get, Json, Router};
use h2::{AsyncConnection, Row};
use serde::Serialize;
use std::sync::Arc;

#[derive(Serialize)]
struct UserItem {
    id: i32,
    name: String,
}

#[derive(Clone)]
struct AppState {
    db: AsyncConnection,
}

async fn list_users(State(state): State<AppState>) -> Json<Vec<UserItem>> {
    let rows = state.db.query("SELECT id, name FROM users LIMIT 50").await.unwrap();
    let users = rows
        .into_iter()
        .map(|r| UserItem {
            id: r.get_as(0).unwrap(),
            name: r.get_as(1).unwrap(),
        })
        .collect();
    Json(users)
}

pub fn create_app(db: AsyncConnection) -> Router {
    Router::new()
        .route("/users", get(list_users))
        .with_state(AppState { db })
}
```

---

# Part VI. サーバー運用とメンテナンス (Server Administration & Maintenance)

## 1. 組み込みと外部接続のハイブリッド運用

同一プロセス内でアプリケーションが組み込みで超高速に読み書きを行いつつ、外部から `psql` や GUI ツールで接続して状態をモニタリングできます。

```rust
let conn = Connection::open("my_system.h2")?;

// バックグラウンドで PostgreSQL リスナーを開始
let server_addr = conn.start_pg_server("0.0.0.0:5432".parse()?).await?;
println!("Database ready. PG-Wire listening on {server_addr}");
```

## 2. DBeaver / DataGrip / psql からの接続

特別なドライバは不要です。標準の PostgreSQL ドライバで接続可能です。

### `psql` コマンドライン接続:
```bash
psql -h localhost -p 5432 -U postgres -d mydb
```

### GUI ツール (DBeaver, TablePlus, DataGrip, VSCode Database Client):
- **接続タイプ**: `PostgreSQL`
- **Host**: `localhost` (またはサーバーの IP)
- **Port**: `5432`
- **Database**: 任意（例: `mydb`）
- **Username**: 任意（例: `postgres`）
- **Password**: 任意（不要）

接続後、テーブルツリーの閲覧、テーブル構造の表示、SQL クエリエディタでの実行、結果グリッド表示がそのまま利用できます。

## 3. 対話型 CLI シェル (`h2-cli`) の使い方

スタンドアロンの CLI バイナリを使用して、データベースファイルやインメモリ DB と対話的に操作できます。

```bash
# データベースファイルを指定して起動（省略時はインメモリ :memory:）
cargo run -p h2-cli -- mydb.h2
```

```text
Connected to h2 database at: mydb.h2
Type SQL queries ending with ';' or '.exit' to quit.
h2> CREATE TABLE users (id INT PRIMARY KEY, name VARCHAR);
Query executed in 1.2ms. (DDL)
h2> INSERT INTO users VALUES (1, 'Alice'), (2, 'Bob');
Query executed in 0.8ms. (2 rows affected)
h2> SELECT * FROM users;
+----+-------+
| id | name  |
+----+-------+
| 1  | Alice |
| 2  | Bob   |
+----+-------+
2 rows in set (0.5ms).
h2> .exit
Bye!
```

## 4. ストレージのコンパクション (Vacuum によるファイル縮小)

追記型 CoW B-Tree ストレージ（MVStore）では、データの更新や削除を繰り返すと過去バージョンの古いデータがファイル内に残ります。
`VACUUM` を実行することで、現在有効なデータのみを前方へ再配置し、ファイルサイズを物理的に縮小（空き領域を OS へ返却）できます。

### SQL からの実行:
```sql
VACUUM;
```

### Rust API からの実行:
```rust
// 同期 API
conn.vacuum()?;

// 非同期 API
async_conn.vacuum().await?;
```

---

# Part VII. SQL コマンド & 関数リファレンス (Command Reference)

### サポートされている SQL 文

| コマンド | 構文例 | 概要 |
| :--- | :--- | :--- |
| `CREATE TABLE` | `CREATE TABLE [IF NOT EXISTS] tbl (col type, ...)` | テーブル作成（主キー、NOT NULL、デフォルト値） |
| `ALTER TABLE` | `ALTER TABLE tbl RENAME TO new_tbl` / `ADD [COLUMN] col_def` / `DROP [COLUMN] col_name` | テーブル定義の変更（リネーム、列追加、列削除） |
| `DROP TABLE` | `DROP TABLE [IF EXISTS] tbl` | テーブルおよび関連インデックス・マップの削除 |
| `TRUNCATE TABLE` | `TRUNCATE TABLE tbl` | 全行およびインデックスの高速一括削除・Row ID リセット |
| `CREATE INDEX` | `CREATE [UNIQUE] INDEX [IF NOT EXISTS] idx ON tbl (col)` | セカンダリ・一意インデックスの作成 |
| `DROP INDEX` | `DROP INDEX [IF EXISTS] idx` | インデックスの削除 |
| `INSERT` | `INSERT INTO tbl [(cols)] VALUES (vals), ...` | 行の挿入 |
| `UPDATE` | `UPDATE tbl SET col = expr, ... [WHERE cond]` | 行の更新（自己参照式、複数列対応） |
| `DELETE` | `DELETE FROM tbl [WHERE cond]` | 行の削除 |
| `SELECT` | `SELECT [DISTINCT] expr [AS alias], ... [FROM tbl [JOIN ...]] [WHERE ...] [GROUP BY ...] [HAVING ...] [ORDER BY ...] [LIMIT ... OFFSET ...]` | データの問い合わせ・集計・結合（FROM なし計算、派生テーブルサブクエリ対応） |
| `UNION` / `UNION ALL` | `SELECT ... UNION [ALL] SELECT ...` | 複数クエリ結果の縦方向結合（重複排除 / 保持） |
| `EXPLAIN` | `EXPLAIN SELECT ...` | クエリ実行計画の確認（IndexScan / TableScan / NestedLoopJoin など） |
| `SHOW TABLES` | `SHOW TABLES;` | 定義されている全テーブル名の一覧照会 |
| `SHOW COLUMNS` | `SHOW COLUMNS FROM tbl;` | テーブルのカラム名・データ型・制約情報の一覧照会 |
| `BEGIN` | `BEGIN;` / `START TRANSACTION;` | トランザクションの開始 |
| `COMMIT` | `COMMIT;` / `END;` | トランザクションのコミット確定 |
| `ROLLBACK` | `ROLLBACK;` | トランザクションのロールバック破棄 |
| `VACUUM` | `VACUUM;` | ストレージのガベージ回収とファイル縮小 |

### 組み込み関数・演算子

| 関数 / 演算子 | 使用例 | 説明 |
| :--- | :--- | :--- |
| `COUNT`, `SUM`, `AVG`, `MIN`, `MAX` | `SELECT department, AVG(salary) FROM emp GROUP BY department` | 標準集約関数 |
| `COALESCE` | `COALESCE(col1, col2, 'default')` | 最初の非 NULL 引数を返却 |
| `UPPER`, `LOWER` | `UPPER(name)`, `LOWER(email)` | 大文字・小文字変換 |
| `CONCAT` | `CONCAT(first_name, ' ', last_name)` | 複数文字列の結合 |
| `LENGTH`, `CHAR_LENGTH` | `LENGTH(title)` | 文字列の文字数を取得 |
| `ABS` | `ABS(amount)` | 数値の絶対値を計算 |
| `NOW`, `CURRENT_TIMESTAMP` | `SELECT NOW()` | 現在の協定世界時（RFC 3339 形式）を取得 |
| `CASE WHEN` | `CASE WHEN age >= 20 THEN 'adult' ELSE 'minor' END` | 条件分岐評価式 |
| `IN`, `NOT IN` | `id IN (1, 2, 3)` / `id IN (SELECT user_id FROM orders)` | リストまたはサブクエリに含まれるか判定 |
| `BETWEEN` | `price BETWEEN 100 AND 500` | 範囲内判定（境界含む） |
| `EXISTS` | `WHERE EXISTS (SELECT 1 FROM orders WHERE ...)` | 相関/非相関サブクエリの存在判定 |
| `FT_SEARCH` | `FT_SEARCH(col, 'キーワード')` | 日本語 2-gram 全文検索述語（AND一致） |
| `FT_SEARCH_MORPH` | `FT_SEARCH_MORPH(col, '形態素 単語')` | 文字種境界形態素解析 全文検索述語 |
| `->` | `data -> 'profile'` | JSON オブジェクトの特定キー抽出（JSON値返却） |
| `->>` | `data ->> 'name'` | JSON オブジェクトの特定キー抽出（テキスト文字列返却） |
| `JSON_EXTRACT` | `JSON_EXTRACT(data, '$.user.email')` | JSON パス式によるプロパティ抽出 |
| `LIKE` | `col LIKE '%test%'` | パターンマッチング |
| `+`, `-`, `*`, `/`, `%` | `price * 1.1` | 四則演算および剰余 |
| `=`, `!=`, `<>`, `<`, `<=`, `>`, `>=` | `age >= 18` | 比較演算子（異なる数値型同士のクロス比較対応） |
| `IS NULL`, `IS NOT NULL` | `optional_col IS NOT NULL` | NULL 検証 |
| `AND`, `OR`, `NOT` | `a > 10 AND NOT (b = 0)` | 論理演算子 |

---

このガイドに関するご質問や新機能の要望は、Issue または Pull Request にてお気軽にお寄せください。
Happy coding with **H2 Database in Rust**!
