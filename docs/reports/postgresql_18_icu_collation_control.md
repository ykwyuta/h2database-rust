# PostgreSQL 18 の ICU による照合順序制御

調査日: 2026-09-26。対象は PostgreSQL 18 の公式ドキュメント。SQL 例は仕様を示すもので、このリポジトリまたは PostgreSQL 実機での実行結果ではない。

## 要約

PostgreSQL の照合順序は文字列の**並び順**に加え、比較、等価性、文字分類、大文字・小文字変換などに関わる。ICU を使うには、サーバーが ICU 対応でビルドされている必要がある。データベース既定の ICU ロケールは作成時に選び、個別の照合順序は `CREATE COLLATION ... PROVIDER = icu` で定義する。列、式、索引には `COLLATE` で適用する。[照合順序の概念](https://www.postgresql.org/docs/18/collation.html)、[CREATE DATABASE](https://www.postgresql.org/docs/18/sql-createdatabase.html)、[CREATE COLLATION](https://www.postgresql.org/docs/18/sql-createcollation.html)

`DETERMINISTIC = false` はバイト列が異なる文字列を等しいと扱えるようにする設定で、大小文字・アクセント・Unicode 正規化形の差を無視したい場合に ICU のロケール指定と組み合わせる。**このフラグだけで大小文字を無視するわけではない**。ICU 側の比較レベルなども指定する。[非決定的照合順序](https://www.postgresql.org/docs/18/collation.html#COLLATION-NONDETERMINISTIC)、[CREATE COLLATION](https://www.postgresql.org/docs/18/sql-createcollation.html)

## 1. 指定する単位と SQL

| 単位 | 指定方法 | 要点 |
| --- | --- | --- |
| クラスターの初期既定 | `initdb --locale-provider=icu --icu-locale=ja-JP -D <data-dir>` | ICU 対応ビルドが必要。個別データベース作成時に変更可能。[initdb](https://www.postgresql.org/docs/18/app-initdb.html) |
| データベース既定 | `CREATE DATABASE appdb TEMPLATE template0 LOCALE_PROVIDER icu ICU_LOCALE 'ja-JP';` | 既存テンプレートとエンコーディング・ロケールを変える場合は `template0` を使う。データベース既定には非決定的比較を設定できない。[CREATE DATABASE](https://www.postgresql.org/docs/18/sql-createdatabase.html) |
| 名前付き照合順序 | `CREATE COLLATION ja_icu (provider = icu, locale = 'ja-JP');` | 既定の provider は `libc`。ICU を使うなら明示する。ユーザー定義照合順序はユーザースキーマに置く。[CREATE COLLATION](https://www.postgresql.org/docs/18/sql-createcollation.html)、[照合順序の管理](https://www.postgresql.org/docs/18/collation.html#COLLATION-MANAGING) |
| 列 | `CREATE TABLE items (name text COLLATE ja_icu);` | 列の照合順序が、その列を参照する式の暗黙の照合順序になる。[照合順序の概念](https://www.postgresql.org/docs/18/collation.html#COLLATION-CONCEPTS) |
| 式・問い合わせ | `SELECT name FROM items ORDER BY name COLLATE ja_icu;` | 明示的な `COLLATE` は暗黙の列照合順序より優先。異なる明示照合順序の衝突はエラーになる。[照合順序の概念](https://www.postgresql.org/docs/18/collation.html#COLLATION-CONCEPTS) |
| 索引 | `CREATE INDEX items_name_ja_idx ON items (name COLLATE ja_icu);` | 索引キーに列既定とは異なる照合順序を指定できる。照合順序は索引の順序・等価性に関係する。[CREATE INDEX](https://www.postgresql.org/docs/18/sql-createindex.html) |

`initdb` は ICU のロケールから事前定義の照合順序を `pg_collation` に登録する。名前には `-x-icu` が付き、例えば `und-x-icu` は ICU の root 照合順序である。利用可能な名前は `SELECT * FROM pg_collation` または psql の `\dOS+` で確認する。同じ動作に見える別の照合順序オブジェクトも PostgreSQL では別物として扱うため、名前を混在させない。[事前定義の ICU 照合順序](https://www.postgresql.org/docs/18/collation.html#COLLATION-PREDEFINED-ICU)、[照合順序の互換性](https://www.postgresql.org/docs/18/collation.html#COLLATION-PREDEFINED)

## 2. ICU ロケールによる制御

ICU ロケールは BCP 47 言語タグで指定する。`-u-` 以下のキーで比較ルールを変えられ、さらに `RULES` で独自の順序規則を追加できる。[ICU カスタム照合順序](https://www.postgresql.org/docs/18/collation.html#COLLATION-ICU-CUSTOM)、[CREATE COLLATION](https://www.postgresql.org/docs/18/sql-createcollation.html)

| 用途 | 例 | 意味 |
| --- | --- | --- |
| ドイツ語の電話帳順 | `de-u-co-phonebk` | `co` で照合方式を選ぶ。 |
| 数字を数値として並べる | `und-u-kn-true` | `kn` により、文字列中の数字を桁の並びではなく数値として比較する。 |
| 大文字を先に並べる | `und-u-kf-upper` | `kf` で大小文字の順序を指定する。 |
| 大小文字を同一視 | `und-u-ks-level2` + `DETERMINISTIC = false` | `ks=level2` では基底文字とアクセントの差を残し、大小文字の差を無視する。 |
| 大小文字とアクセントを同一視 | `und-u-ks-level1` + `DETERMINISTIC = false` | `ks=level1` では基底文字だけを比較する。 |
| 独自規則 | `locale = 'und', rules = '&V << w <<< W'` | ICU の tailoring rules を追加する。 |

上のキーの意味と例は PostgreSQL 18 の [ICU 比較レベル・設定](https://www.postgresql.org/docs/18/collation.html#COLLATION-ICU-CUSTOM)および[ルール指定](https://www.postgresql.org/docs/18/collation.html#COLLATION-ICU-RULES)に基づく。ロケールごとに既定値が違う場合があり、実際の文字列集合で期待する順序・等価性を検証する。

```sql
CREATE COLLATION und_ci (
    provider = icu,
    locale = 'und-u-ks-level2',
    deterministic = false
);

-- 大小文字を同一視する比較・一意性に利用
SELECT 'AbC' = 'abc' COLLATE und_ci;
CREATE TABLE accounts (email text);
CREATE UNIQUE INDEX accounts_email_ci_idx ON accounts (email COLLATE und_ci);

-- ドイツ語の電話帳順
CREATE COLLATION de_phonebook (provider = icu, locale = 'de-u-co-phonebk');
SELECT name FROM items ORDER BY name COLLATE de_phonebook;
```

この `und_ci` の比較と索引は意図する挙動を示す**実行例**であり、この調査では実測していない。列全体を非決定的照合順序にすることもできるが、適用範囲が広がるので比較対象の列・索引を選んで指定する。[非決定的照合順序](https://www.postgresql.org/docs/18/collation.html#COLLATION-NONDETERMINISTIC)

## 3. 制約・性能・運用

| 観点 | PostgreSQL 18 の仕様・注意点 |
| --- | --- |
| パターン照合 | `LIKE` は非決定的照合順序に対応する。`ILIKE`、`SIMILAR TO`、POSIX 正規表現は対応しない。`LIKE` の `_` は照合順序と無関係に常に 1 文字に一致する。[パターン照合](https://www.postgresql.org/docs/18/functions-matching.html) |
| 索引とコスト | 非決定的照合順序には性能上のコストがあり、その照合順序を使う B-tree 索引では重複排除を利用できない。[非決定的照合順序](https://www.postgresql.org/docs/18/collation.html#COLLATION-NONDETERMINISTIC)、[B-tree 索引](https://www.postgresql.org/docs/18/btree.html) |
| エンコーディング | ICU が対応しないデータベースエンコーディングでは、ICU 照合順序が利用できない。[事前定義の ICU 照合順序](https://www.postgresql.org/docs/18/collation.html#COLLATION-PREDEFINED-ICU) |
| ロケール更新 | ICU の更新で照合順序が変わると、既存索引の順序が不整合になり得る。照合順序の記録済みバージョンと実際のバージョンを照合し、影響を受けるオブジェクトを再構築した**後**に `ALTER COLLATION ... REFRESH VERSION` を実行する。この操作自体は再構築を検証しない。[ALTER COLLATION](https://www.postgresql.org/docs/18/sql-altercollation.html) |
| データベース既定の更新 | データベース既定の照合順序には `ALTER DATABASE ... REFRESH COLLATION VERSION` を使う。こちらも影響を受けるオブジェクトの再構築を先に確認する。[ALTER COLLATION](https://www.postgresql.org/docs/18/sql-altercollation.html)、[ALTER DATABASE](https://www.postgresql.org/docs/18/sql-alterdatabase.html) |

以下はバージョン確認の例。`pg_collation` は provider (`i` = ICU)、非決定性、ロケール、バージョンを保持する。差分が出た場合、単に `REFRESH VERSION` で警告を消さず、依存オブジェクトを確認して必要な索引を `REINDEX` 等で再構築する。[pg_collation](https://www.postgresql.org/docs/18/catalog-pg-collation.html)、[pg_database](https://www.postgresql.org/docs/18/catalog-pg-database.html)、[ALTER COLLATION](https://www.postgresql.org/docs/18/sql-altercollation.html)

```sql
SELECT collname, collprovider, collisdeterministic, colllocale,
       collversion, pg_collation_actual_version(oid) AS actual_version
FROM pg_collation
WHERE collprovider = 'i';

SELECT datname, datlocprovider, datlocale, datcollversion,
       pg_database_collation_actual_version(oid) AS actual_version
FROM pg_database
WHERE datname = current_database();
```

## 4. このリポジトリへの示唆

現行コードの静的確認では、[`ColumnDef`](../../crates/h2-sql/src/catalog.rs) に照合順序メタデータはなく、[`Value::String` の等価性・順序](../../crates/h2-types/src/value.rs) は Rust 文字列の直接比較、[`ORDER BY` の実行経路](../../crates/h2-sql/src/executor.rs) は `Value::partial_cmp` を用いている。`crates/` で `icu` / `collation` / `collate` の専用実装も確認できなかった。したがって、本報告の PostgreSQL 向け SQL 例が現行 H2 Rust で動くとは判断しない。これはコード上の調査結果であり、実行テストによる網羅的な非対応判定ではない。

互換機能を検討する場合は、(1) 照合順序を列・式・索引に保持する、(2) 比較・等価性・一意性・検索・ソートで同じ照合器を使う、(3) ICU ロケールとバージョンを永続化して変更時に索引を再構築する、の順に設計する必要がある。SQL の構文受付だけを先行させると、並び順や一意制約について誤った結果を返し得る。

## 参照した公式資料

- [PostgreSQL 18: Collation Support](https://www.postgresql.org/docs/18/collation.html)
- [PostgreSQL 18: CREATE COLLATION](https://www.postgresql.org/docs/18/sql-createcollation.html)
- [PostgreSQL 18: CREATE DATABASE](https://www.postgresql.org/docs/18/sql-createdatabase.html)、[initdb](https://www.postgresql.org/docs/18/app-initdb.html)
- [PostgreSQL 18: Pattern Matching](https://www.postgresql.org/docs/18/functions-matching.html)
- [PostgreSQL 18: ALTER COLLATION](https://www.postgresql.org/docs/18/sql-altercollation.html)、[ALTER DATABASE](https://www.postgresql.org/docs/18/sql-alterdatabase.html)
