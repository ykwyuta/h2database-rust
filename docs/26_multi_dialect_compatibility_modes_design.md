# 多方言 SQL 互換モード (SQL Dialect Compatibility Modes) 設計書

## 1. 概要と背景

本家 Java 版 H2 Database は、単一の組み込み・サーバー型 RDBMS でありながら、他の主要商用・オープンソース RDBMS との高い互換性を維持するために **互換モード（Compatibility Modes: `SET MODE <name>`）** を提供しています。

`h2database-rust` においても、以下の主要な商用・オープンソースデータベースおよび標準独自モードを対象とした「多方言 SQL 互換エンジン」を導入します：
- **`REGULAR`** (標準独自モード: H2 デフォルト)
- **`PostgreSQL`** (PostgreSQL 互換)
- **`MySQL`** (MySQL / MariaDB 互換)
- **`Oracle`** (Oracle Database 互換)
- **`MSSQLServer`** (Microsoft SQL Server / T-SQL 互換)
- **`DB2`** (IBM DB2 互換)

※ Derby や HSQLDB、SQLite などのマイナー・特定組み込み向け互換機能はスコープ外とし、主要エンタープライズ RDBMS 5 種 ＋ 標準モードの 6 体系に絞って高密度・高品質な互換性を提供します。

これにより、既存のアプリケーション資産（Hibernate, MyBatis, Spring Data, 各種 ORM や SQL スクリプト）を変更することなく、`h2database-rust` を代替データベースまたはインメモリ検証用 DB として柔軟に利用可能にします。

---

## 2. 互換モード一覧と特性マッピング

| モード名 (`SET MODE`) | 主なエイリアス | パーサー方言 (`sqlparser`) | 特有のデータ型 | 特有の組み込み関数 | 特有の構文・セマンティクス |
| :--- | :--- | :--- | :--- | :--- | :--- |
| **`REGULAR`** (デフォルト) | `H2`, `DEFAULT` | `GenericDialect` | 全標準型 | 標準関数 | 標準 SQL、`FROM DUAL` 許容 |
| **`PostgreSQL`** | `Postgres`, `PG` | `PostgreSqlDialect` | `BYTEA`, `TIMESTAMPTZ`, `SERIAL`, `BIGSERIAL`, `JSON`, `UUID` | PGWire, `pg_proc` 仮想表, `random()` | `::type` キャスト, `ILIKE`, PL/pgSQL |
| **`MySQL`** | `MariaDB` | `MySqlDialect` | `DATETIME`, `TINYINT`, `MEDIUMINT`, `LONGTEXT`, `LONGBLOB`, `BIT` | `IFNULL`, `IF`, `CURDATE`, `CURTIME`, `UNIX_TIMESTAMP`, `FROM_UNIXTIME`, `CONCAT`, `CONCAT_WS`, `DATABASE` | バッククォート識別子 (`` `col` ``), `LIMIT offset, count` |
| **`Oracle`** | - | `GenericDialect` | `VARCHAR2`, `NVARCHAR2`, `NUMBER`, `RAW`, `CLOB`, `BLOB`, `DATE` | `NVL`, `NVL2`, `DECODE`, `SYSDATE`, `INSTR`, `TO_CHAR`, `TO_DATE`, `TO_NUMBER` | `FROM DUAL` 必須/許容, 空文字 `''` の `NULL` 扱い |
| **`MSSQLServer`** | `SQLServer`, `MSSQL`, `T-SQL` | `MsSqlDialect` | `NVARCHAR`, `NCHAR`, `DATETIME`, `DATETIME2`, `SMALLDATETIME`, `IMAGE`, `MONEY`, `UNIQUEIDENTIFIER` | `ISNULL`, `GETDATE`, `LEN`, `CHARINDEX`, `NEWID`, `SQUARE` | ブラケット識別子 (`[col]`), `+` による文字列連結 |
| **`DB2`** | - | `AnsiDialect` | `VARCHAR`, `TIMESTAMP` | 標準関数 | ANSI 準拠、`FROM DUAL` 許容 |

---

## 3. アーキテクチャ設計

```
              ┌────────────────────────────────────────────────────────┐
              │           Client (PGWire / Embedded / CLI)             │
              └───────────────────────────┬────────────────────────────┘
                                          │ SQL Query / "SET MODE <X>"
                                          ▼
              ┌────────────────────────────────────────────────────────┐
              │               Session / Connection State               │
              │         current_mode: Arc<RwLock<SqlDialectMode>>       │
              └───────────────────────────┬────────────────────────────┘
                                          │
                   ┌──────────────────────┴──────────────────────┐
                   ▼                                             ▼
     ┌───────────────────────────┐                 ┌───────────────────────────┐
     │ Dynamic Dialect Parser    │                 │ Execution & Expression    │
     │ - PostgreSqlDialect       │                 │ - Data Type Normalization │
     │ - MySqlDialect (``)       │                 │ - Dialect Functions (NVL, │
     │ - MsSqlDialect ([])       │                 │   IFNULL, ISNULL, DECODE) │
     │ - AnsiDialect (DB2)       │                 │ - '+' Concat in MSSQL     │
     │ - GenericDialect (H2/Ora) │                 │ - Empty-string-as-NULL    │
     └───────────────────────────┘                 │ - DUAL virtual table      │
                                                   └───────────────────────────┘
```

### 3.1 `SqlDialectMode` 列挙型
`h2-types::mode` に定義：
```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum SqlDialectMode {
    #[default]
    Regular,
    PostgreSql,
    MySql,
    Oracle,
    MsSqlServer,
    Db2,
}
```

各モードは以下のメタデータ・ビヘイビアフラグを提供：
- `fn parse_str(s: &str) -> Option<Self>`: 大文字小文字やエイリアス（`Postgres`, `MariaDB`, `SQLServer`, `T-SQL` など）を正規化判定
- `fn empty_strings_are_null(&self) -> bool`: Oracle モードの場合 `true`
- `fn plus_as_string_concat(&self) -> bool`: MSSQLServer モードの場合 `true`
- `fn support_dual_table(&self) -> bool`: Oracle, MySQL, Regular, DB2 で `true`

### 3.2 動的パーサー切り替え (`crates/h2-sql/src/parser.rs`)
SQL の構文解析時に、セッションの `SqlDialectMode` に応じた `sqlparser::dialect::Dialect` を動的に選択：
- `SqlDialectMode::PostgreSql` -> `sqlparser::dialect::PostgreSqlDialect`
- `SqlDialectMode::MySql` -> `sqlparser::dialect::MySqlDialect`
- `SqlDialectMode::MsSqlServer` -> `sqlparser::dialect::MsSqlDialect`
- `SqlDialectMode::Db2` -> `sqlparser::dialect::AnsiDialect`
- `SqlDialectMode::Regular` / `Oracle` -> `sqlparser::dialect::GenericDialect`（必要に応じて方言フォールバック）

### 3.3 データ型の多方言正規化 (`convert_data_type`)
対象 RDBMS の代表的な型名を内部型 `DataType` へ透過的に変換：
- **Oracle**:
  - `VARCHAR2(n)`, `NVARCHAR2(n)` -> `DataType::VarChar(Some(n))`
  - `NUMBER`, `NUMBER(p, s)` -> `DataType::Decimal(p, s)` または `BigInt` / `Integer`
  - `RAW(n)` -> `DataType::Binary(Some(n))`
  - `CLOB` -> `DataType::VarChar(None)`
  - `BLOB` -> `DataType::Blob`
- **MySQL**:
  - `DATETIME` -> `DataType::Timestamp`
  - `TINYINT` -> `DataType::TinyInt`
  - `MEDIUMINT` -> `DataType::Integer`
  - `LONGTEXT`, `MEDIUMTEXT`, `TINYTEXT` -> `DataType::VarChar(None)`
  - `LONGBLOB`, `MEDIUMBLOB`, `TINYBLOB` -> `DataType::Blob`
  - `BIT` -> `DataType::Boolean`
- **MSSQLServer**:
  - `NVARCHAR(n)`, `NCHAR(n)` -> `DataType::VarChar(Some(n))`, `DataType::Char(n)`
  - `DATETIME`, `DATETIME2`, `SMALLDATETIME` -> `DataType::Timestamp`
  - `IMAGE` -> `DataType::Blob`
  - `MONEY`, `SMALLMONEY` -> `DataType::Decimal(19, 4)`
  - `UNIQUEIDENTIFIER` -> `DataType::Uuid`
  - `VARBINARY(n)` -> `DataType::Binary(Some(n))`

### 3.4 方言関数の拡張 (`crates/h2-sql/src/expression.rs`)
各 RDBMS で頻出する関数をビルトイン式評価に追加：
1. **NULL 代替関数**:
   - `NVL(expr1, expr2)` (Oracle)
   - `IFNULL(expr1, expr2)` (MySQL)
   - `ISNULL(expr1, expr2)` (MSSQL)
   - `NVL2(expr1, expr2, expr3)` (Oracle: `expr1 IS NOT NULL ? expr2 : expr3`)
2. **条件分岐・論理関数**:
   - `DECODE(val, s1, r1, [s2, r2, ...], [def])` (Oracle)
   - `IF(cond, expr1, expr2)` (MySQL)
3. **日付・時刻関数**:
   - `SYSDATE` (Oracle: `CURRENT_TIMESTAMP` を返却)
   - `GETDATE()` (MSSQL: `CURRENT_TIMESTAMP` を返却)
   - `CURDATE()`, `CURRENT_DATE()` (MySQL)
   - `CURTIME()`, `CURRENT_TIME()` (MySQL)
   - `UNIX_TIMESTAMP([date])`, `FROM_UNIXTIME(epoch)` (MySQL)
4. **文字列・補助関数**:
   - `LEN(str)` (MSSQL: `LENGTH(str)`)
   - `CHARINDEX(sub, str, [start])` (MSSQL)
   - `INSTR(str, sub, [start])` (Oracle / MySQL)
   - `CONCAT(a, b, ...)` (MySQL)
   - `CONCAT_WS(sep, a, b, ...)` (MySQL)
   - `NEWID()` (MSSQL: UUID 生成)
   - `SQUARE(x)` (MSSQL: `x * x`)
   - `DATABASE()`, `SCHEMA()`, `VERSION()` (MySQL)

### 3.5 仮想テーブル `DUAL` のサポート
Oracle や MySQL で一般的な `SELECT ... FROM DUAL` を実行可能にするため、テーブル解決処理において `table_name.eq_ignore_ascii_case("dual")` の場合、ダミー列 `dummy VARCHAR(1)` と 1 行 `Row(['X'])` を持つ仮想テーブルとして即座に解決。

### 3.6 セッション操作構文
- `SET MODE <MODE_NAME>`: セッションの方言モードを即時変更。
- `SHOW MODE`: 現在の方言モード名（大文字）を 1 列 1 行で返却。
- `SELECT CURRENT_MODE()`: 現在の方言モード名を返却。
- 接続オプション: `Connection::open_with_mode(path, mode)` や `Connection::open_in_memory_with_mode(mode)`。

---

## 4. 移行・互換性・テスト検証計画

1. **基本切り替えテスト**:
   - `SET MODE MySQL`, `SET MODE Oracle`, `SET MODE MSSQLServer`, `SET MODE DB2`, `SET MODE PostgreSQL`, `SET MODE REGULAR` の構文受容と `SHOW MODE` の整合確認。
2. **MySQL モード検証**:
   - バッククォート識別子 `` CREATE TABLE `users` (`id` INT, `created_at` DATETIME) ``
   - `IFNULL('a', 'b')`, `IF(1 > 0, 'yes', 'no')`, `UNIX_TIMESTAMP()`, `CONCAT('a', 'b', 'c')`
3. **Oracle モード検証**:
   - `CREATE TABLE emp (empno NUMBER(5), ename VARCHAR2(50), hiredate DATE)`
   - `SELECT NVL(null, 100), NVL2('ok', 1, 2), DECODE('B', 'A', 1, 'B', 2, 3), SYSDATE FROM DUAL`
   - 空文字 `''` が `NULL` として評価されることの検証。
4. **MSSQLServer モード検証**:
   - ブラケット識別子 `CREATE TABLE [orders] ([id] INT, [cost] MONEY)`
   - `SELECT ISNULL(null, 'default'), GETDATE(), LEN('hello'), SQUARE(4)`
   - `'Hello' + ' ' + 'World'` による文字列連結。
5. **DB2 モード検証**:
   - ANSI 標準構文と `FROM DUAL` 許容の検証。
6. **リグレッション確認**:
   - 既存の PostgreSQL モードおよび PL/pgSQL、TPROC-C ベンチマーク互換性に一切影響を与えないことを保証。
