# SQL Server の照合順序制御

調査日: 2026-09-26。主対象は Microsoft Learn の **SQL Server 2025 (17.x)** 向け公式資料。Azure SQL Database / Managed Instance には別の制約があるため、該当箇所で区別する。以下の SQL は説明用であり、SQL Server 実機での実行結果ではない。

## 要約

SQL Server の照合順序は、文字列の**並び順・比較・等価性**、大小文字・アクセント・かな・全半角などの区別を決める。`char` / `varchar` などではコードページにも関わる。サーバー、データベース、列、式の単位で指定でき、異なる照合順序を同一データベース内で併用できる。[Collation and Unicode support](https://learn.microsoft.com/en-us/sql/relational-databases/collations/collation-and-unicode-support?view=sql-server-ver17)

PostgreSQL の [ICU 照合順序報告](./postgresql_18_icu_collation_control.md)で扱った BCP 47 ロケール名と、SQL Server の Windows / `SQL_*` 照合順序名は別体系である。SQL Server の `SQL_*` 照合順序は旧版との互換用で、Microsoft は新規開発での使用を推奨していない。[SQL Server Collation Name](https://learn.microsoft.com/en-us/sql/t-sql/statements/sql-server-collation-name-transact-sql?view=sql-server-ver17)、[Collation and Unicode support](https://learn.microsoft.com/en-us/sql/relational-databases/collations/collation-and-unicode-support?view=sql-server-ver17)

## 1. 名前の読み方

Windows 照合順序名はロケール・照合規則と感度オプションを組み合わせる。**存在する名前を任意の接尾辞から合成できるわけではない**ため、対象インスタンスの `sys.fn_helpcollations()` で使用可能な名前を確認する。[Windows collation name](https://learn.microsoft.com/en-us/sql/t-sql/statements/windows-collation-name-transact-sql?view=sql-server-ver17)、[sys.fn_helpcollations](https://learn.microsoft.com/en-us/sql/relational-databases/system-functions/sys-fn-helpcollations-transact-sql?view=sql-server-ver17)

| 要素 | 意味 | 注意 |
| --- | --- | --- |
| `_CI` / `_CS` | 大小文字を区別しない / する | 比較・ソートの結果に影響する。 |
| `_AI` / `_AS` | アクセントを区別しない / する | 例: `a` と `á` の扱い。 |
| `_KS` | ひらがなとカタカナを区別 | 省略時はかなの差を無視。 |
| `_WS` | 全角と半角を区別 | 省略時は幅の差を無視。 |
| `_VSS` | 異体字セレクターを区別 | 対応する日本語 140 系で使用。フルテキスト索引・XML・CLR には制約がある。 |
| `_BIN` / `_BIN2` | バイナリ順 / コードポイント順 | `_BIN` は旧方式。Unicode の純粋なコードポイント比較には `_BIN2` を選ぶ。 |
| `_SC` | 補助文字への対応を表す名称要素 | SQL Server 2017 以降の新しい照合順序では補助文字対応が標準。 |
| `_UTF8` | 対応する `char` / `varchar` を UTF-8 で格納 | `nchar` / `nvarchar` の格納形式は変わらない。2019 (15.x) 以降。 |

感度・バイナリ指定の定義は [Collation and Unicode support](https://learn.microsoft.com/en-us/sql/relational-databases/collations/collation-and-unicode-support?view=sql-server-ver17) を参照。`Latin1_General_100_CI_AS_SC_UTF8` は、Windows 系、大小文字非区別、アクセント区別、補助文字対応、`varchar` の UTF-8 格納を表す例。`SQL_Latin1_General_CP1_CI_AS` は旧 `SQL_*` 系であり、名前の類似性だけで同じ比較規則とみなせない。[Collation and Unicode support](https://learn.microsoft.com/en-us/sql/relational-databases/collations/collation-and-unicode-support?view=sql-server-ver17)

## 2. 適用範囲と指定方法

| 範囲 | SQL Server の指定・継承 | 変更時の要点 |
| --- | --- | --- |
| サーバー（インスタンス） | セットアップ時に選択。新規 DB とシステム DB の既定になる。 | 変更にはシステム DB の再構築、ユーザー DB の再作成・データ再投入などが必要で、既存 DB の照合順序は自動変更されない。[Set or change the server collation](https://learn.microsoft.com/en-us/sql/relational-databases/collations/set-or-change-the-server-collation?view=sql-server-ver17) |
| データベース | `CREATE DATABASE ... COLLATE name`。省略時はサーバー既定。 | `ALTER DATABASE ... COLLATE name` は既存ユーザーテーブル列の照合順序を変更しない。スキーマ依存物などが変更を妨げる場合もある。[Set or change the database collation](https://learn.microsoft.com/en-us/sql/relational-databases/collations/set-or-change-the-database-collation?view=sql-server-ver17)、[ALTER DATABASE](https://learn.microsoft.com/en-us/sql/t-sql/statements/alter-database-transact-sql?view=sql-server-ver17) |
| 列 | `CREATE TABLE ... name nvarchar(100) COLLATE name`。省略時は DB 既定。 | `ALTER TABLE ... ALTER COLUMN ... COLLATE` で変更する。索引、統計、計算列、CHECK、外部キー等の依存を事前に解消する必要がある。[Set or change the column collation](https://learn.microsoft.com/en-us/sql/relational-databases/collations/set-or-change-the-column-collation?view=sql-server-ver17) |
| 式・問い合わせ | `expression COLLATE name`。比較、`LIKE`、`ORDER BY` などに適用。 | `COLLATE` 名はリテラル指定であり、変数や式では渡せない。[COLLATE](https://learn.microsoft.com/en-us/sql/t-sql/statements/collations?view=sql-server-ver17) |

```sql
-- インスタンスに存在する照合順序を先に調べる
SELECT name, description
FROM sys.fn_helpcollations()
WHERE name LIKE N'Latin1_General_100_CI_AS%';

CREATE DATABASE CollationDemo COLLATE Latin1_General_100_CI_AS_SC_UTF8;
GO
USE CollationDemo;
GO
CREATE TABLE dbo.Customers (
    CustomerId int NOT NULL PRIMARY KEY,
    DisplayName nvarchar(100) COLLATE Latin1_General_100_CI_AS_SC NOT NULL
);

-- この比較では大小文字を区別する
SELECT CustomerId
FROM dbo.Customers
WHERE DisplayName COLLATE Latin1_General_100_CS_AS_SC = N'Alice';
```

`GO` はクライアントのバッチ区切りであり、T-SQL の文ではない。この例の `nvarchar` 列は `_UTF8` による格納変更の対象ではない。[COLLATE](https://learn.microsoft.com/en-us/sql/t-sql/statements/collations?view=sql-server-ver17)、[Collation and Unicode support](https://learn.microsoft.com/en-us/sql/relational-databases/collations/collation-and-unicode-support?view=sql-server-ver17)

## 3. 混在時の優先順位と一時表

文字列式の照合順序は、明示 `COLLATE`（`Explicit`） > 列参照（`Implicit`） > リテラル・変数等（`Coercible-default`）の優先順位で決まる。異なる `Explicit` 同士はエラーになり、異なる `Implicit` 同士を組み合わせると `No-collation` となり、比較など照合順序が必要な演算でエラーになる。結合や `UNION` を含む異照合順序のデータ連携では、どちらの規則で比較するか明示する。[Collation precedence](https://learn.microsoft.com/en-us/sql/t-sql/statements/collation-precedence-transact-sql?view=sql-server-ver17)

`tempdb` は通常インスタンス側の照合順序を使うため、異なる既定照合順序のユーザー DB から一時表を作ると、永続表との `JOIN` で衝突し得る。一時表の文字列列に `COLLATE DATABASE_DEFAULT` を指定すると、接続中のユーザー DB の既定を使える。[Set or change the column collation](https://learn.microsoft.com/en-us/sql/relational-databases/collations/set-or-change-the-column-collation?view=sql-server-ver17)、[COLLATE](https://learn.microsoft.com/en-us/sql/t-sql/statements/collations?view=sql-server-ver17)

```sql
CREATE TABLE #CustomerNames (
    DisplayName nvarchar(100) COLLATE DATABASE_DEFAULT NOT NULL
);

-- 列の照合順序が違う場合は、必要な比較規則を式で明示する
SELECT a.CustomerId
FROM dbo.Customers AS a
JOIN dbo.OtherCustomers AS b
  ON a.DisplayName = b.DisplayName COLLATE Latin1_General_100_CI_AS_SC;
```

上の `dbo.OtherCustomers` は説明用の表であり、前節の SQL だけでは作成されない。

## 4. 変更・移行・性能上の注意

1. **変更前に値の意味を再確認する。** 大小文字やアクセントを区別しない規則へ移ると、以前は異なると扱った値が同一視される可能性がある。一意制約、重複データ、検索結果、識別子の衝突を事前検査する。DB 照合順序の変更が既存列を変えないことにも注意する。[Set or change the database collation](https://learn.microsoft.com/en-us/sql/relational-databases/collations/set-or-change-the-database-collation?view=sql-server-ver17)、[ALTER DATABASE](https://learn.microsoft.com/en-us/sql/t-sql/statements/alter-database-transact-sql?view=sql-server-ver17)
2. **列変更の依存と停止時間を計画する。** 索引・統計・計算列・制約がある列は、そのまま照合順序を変更できない。大きな表の `ALTER COLUMN` はブロッキングになり得る。コピーして切り替える方法もあるが、主キー・外部キー・トリガー・最終同期を設計する。[Set or change the column collation](https://learn.microsoft.com/en-us/sql/relational-databases/collations/set-or-change-the-column-collation?view=sql-server-ver17)
3. **UTF-8 は型と長さを合わせて評価する。** `_UTF8` は `varchar` / `char` の格納に影響するが、`nvarchar` / `nchar` は変わらない。文字の分布により使用バイト数が増減するため、既存列を移行する際は `DATALENGTH` などでサイズを確認する。[Collation and Unicode support](https://learn.microsoft.com/en-us/sql/relational-databases/collations/collation-and-unicode-support?view=sql-server-ver17)
4. **問い合わせ側だけの `COLLATE` 変更は実行計画を確認する。** 索引に保存された順序と `ORDER BY` の照合順序が異なると、索引順をそのまま利用できず別途ソートが入る例が公式資料にある。対象クエリの実行計画と読み取り量を測る。[Index JSON data](https://learn.microsoft.com/en-us/sql/relational-databases/json/index-json-data?view=sql-server-ver17)
5. **提供形態を区別する。** Azure SQL Database では論理サーバーの照合順序を変更できず、`ALTER DATABASE ... COLLATE` も使えない。データとカタログの照合順序を DB 作成時に設定する。Azure SQL Managed Instance のサーバー照合順序は作成時に指定し、後から変更できない。[Set or change the server collation](https://learn.microsoft.com/en-us/sql/relational-databases/collations/set-or-change-the-server-collation?view=sql-server-ver17)、[Set or change the database collation](https://learn.microsoft.com/en-us/sql/relational-databases/collations/set-or-change-the-database-collation?view=sql-server-ver17)

## 5. 現状把握に使う SQL

以下は Microsoft の[照合順序情報の照会手順](https://learn.microsoft.com/en-us/sql/relational-databases/collations/view-collation-information?view=sql-server-ver17)に基づく。異なる照合順序が混在する列を先に列挙すると、移行範囲を見積もりやすい。

```sql
SELECT CONVERT(varchar(128), SERVERPROPERTY('Collation')) AS server_collation;

SELECT name, collation_name
FROM sys.databases;

SELECT t.name AS table_name, c.name AS column_name, c.collation_name
FROM sys.columns AS c
JOIN sys.tables AS t ON t.object_id = c.object_id
WHERE c.collation_name IS NOT NULL
ORDER BY t.name, c.name;

SELECT name, description
FROM sys.fn_helpcollations()
WHERE name LIKE N'Japanese%';
```

## このリポジトリへの示唆

SQL Server と互換の照合順序制御を検討するなら、構文だけでなく、列メタデータ、文字列比較、一意性、ソート、索引キー、式の優先順位、エンコーディングを一貫して扱う必要がある。現行 H2 Rust の [`ColumnDef`](../../crates/h2-sql/src/catalog.rs) には照合順序メタデータがなく、[`Value::String` の比較](../../crates/h2-types/src/value.rs) は Rust 文字列の直接比較である。これはコード上の確認であり、SQL Server 互換性を実行試験した結果ではない。

## 参照した主な公式資料

- [Collation and Unicode support](https://learn.microsoft.com/en-us/sql/relational-databases/collations/collation-and-unicode-support?view=sql-server-ver17)
- [COLLATE (Transact-SQL)](https://learn.microsoft.com/en-us/sql/t-sql/statements/collations?view=sql-server-ver17)、[Collation precedence](https://learn.microsoft.com/en-us/sql/t-sql/statements/collation-precedence-transact-sql?view=sql-server-ver17)
- [Set or change the server collation](https://learn.microsoft.com/en-us/sql/relational-databases/collations/set-or-change-the-server-collation?view=sql-server-ver17)、[database collation](https://learn.microsoft.com/en-us/sql/relational-databases/collations/set-or-change-the-database-collation?view=sql-server-ver17)、[column collation](https://learn.microsoft.com/en-us/sql/relational-databases/collations/set-or-change-the-column-collation?view=sql-server-ver17)
- [View collation information](https://learn.microsoft.com/en-us/sql/relational-databases/collations/view-collation-information?view=sql-server-ver17)
