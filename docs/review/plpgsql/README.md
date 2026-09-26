# PostgreSQL 18 公式仕様と PL/pgSQL 互換機能の対応表

確認日: 2026-09-26。比較対象は PostgreSQL **18** の [PL/pgSQL 公式章](https://www.postgresql.org/docs/18/plpgsql.html) と、H2 Rust の [`PlPgSqlSimulator`](../../../crates/h2-sql/src/plpgsql_sim.rs)、[`SQLEngine` の手続き分岐](../../../crates/h2-sql/src/executor.rs)、[既存の疎通記録](../../23_hammerdb_tprocc_performance_evaluation.md) である。ソースを静的に確認した評価であり、この報告のために新しい互換テストは実行していない。

## 判定の読み方

| 表記 | 意味 |
| --- | --- |
| **専用実装** | 固定名の HammerDB TPROC-C 手続き・補助関数を Rust で処理する。任意の PL/pgSQL 本文は実行しない。 |
| **受付のみ** | SQL を成功として受け取るが、PostgreSQL と同じ定義・実行・削除の意味は持たない。 |
| **未対応** | 対応する PL/pgSQL パーサーまたは実行処理が確認できない。通常の SQL に似た機能があっても含めない。 |
| **要検証** | 実装経路はあるが、PostgreSQL 18 と同じ結果・権限・トランザクション境界を示す試験がない。 |

結論として、現行の「PL/pgSQL 互換」は**HammerDB の特定ルーチンを PGWire から呼ぶためのプロトコル・ワークロード互換**である。PostgreSQL 18 の汎用 PL/pgSQL 言語互換とは扱えない。既存のテストは `DBMS_RANDOM` と `neword` / `payment` などの固定ルーチンの登録・呼び出しを確認しているが、任意の関数本文の実行は検証していない（[`plpgsql_sim.rs` のテスト](../../../crates/h2-sql/src/plpgsql_sim.rs)、[Tcl 疎通スクリプト](../../../benches/hammerdb/test_plpgsql_sim_pgwire.tcl)）。

## 1. 定義・呼び出し・カタログ

| PostgreSQL 18 公式機能 | H2 Rust の現状と根拠 | 判定 |
| --- | --- | --- |
| [`CREATE FUNCTION` / `CREATE PROCEDURE`](https://www.postgresql.org/docs/18/sql-createfunction.html) で本文・言語・戻り値を定義 | `CREATE [OR REPLACE]` を文字列で検出し、空の引数情報と戻り値情報を持つ名前だけをメモリ登録。本文と `LANGUAGE` を解析・実行しない。 | 受付のみ |
| [`CREATE OR REPLACE`](https://www.postgresql.org/docs/18/sql-createfunction.html) の定義更新と所有者・戻り値制約 | 同名キーを上書きするだけ。定義本文・所有者・署名がなく、互換制約を検査できない。 | 受付のみ |
| [スキーマ、引数型によるオーバーロード](https://www.postgresql.org/docs/18/sql-createfunction.html) | レジストリは小文字化した名前だけの `HashMap`。引数型、スキーマ、デフォルト引数による解決なし。 | 未対応 |
| [`IN` / `OUT` / `INOUT` / `VARIADIC`、デフォルト引数](https://www.postgresql.org/docs/18/sql-createfunction.html) | `ProcedureDef.parameters` は登録時も空。`CALL` は文字列引数を分割し、固定手続きでは位置と既定値を Rust 側で解釈。OUT 行は固定の列構成。 | 専用実装 |
| [`RETURNS`、`RETURNS SETOF`、`RETURNS TABLE`](https://www.postgresql.org/docs/18/sql-createfunction.html) | `return_type` は `None`。任意の結果型・集合返却を検証/生成する処理なし。 | 未対応 |
| [`STRICT`、volatility、`SECURITY DEFINER` / `INVOKER`、`PARALLEL`、`COST`、`ROWS`](https://www.postgresql.org/docs/18/sql-createfunction.html) | DDL 末尾の属性を保存せず、NULL 入力、権限、最適化への効果も適用しない。`STRICT` と書いた既存テストは登録受付のみを確認。 | 受付のみ |
| [`CALL` と出力パラメータ](https://www.postgresql.org/docs/18/sql-call.html) | `neword`、`payment`、`delivery`、`ostat`、`slev` は専用 Rust 処理で結果行を返す。一般的な登録済み手続きの本文は実行されない。 | 専用実装 |
| 未定義手続きの呼出しエラーと署名照合（[`CALL`](https://www.postgresql.org/docs/18/sql-call.html)） | `execute_call` の既定分岐は登録有無を確認せず `ExecutionResult::Ddl` を返す。未定義名や引数不一致でも成功と見える可能性がある。 | **受付のみ・重大な差分** |
| [`SELECT` からの関数呼出し](https://www.postgresql.org/docs/18/sql-createfunction.html) | `DBMS_RANDOM` と固定 5 名に先頭一致した SELECT のみ特別処理。ユーザー定義関数の本文評価経路なし。 | 専用実装 |
| [`DROP FUNCTION` / `DROP PROCEDURE`](https://www.postgresql.org/docs/18/sql-dropfunction.html) | 実行層は `DROP` を成功として返すだけで、レジストリから削除しない。引数型、依存関係、`CASCADE` も扱わない。 | **受付のみ・重大な差分** |
| [`pg_proc`](https://www.postgresql.org/docs/18/catalog-pg-proc.html) の実カタログ（`prokind`、署名、所有者など） | `FROM PG_PROC` を含む SELECT は固定の `prokind='p', cnt=5` の 1 行へ短絡する。登録内容を反映したカタログではない。 | 受付のみ |
| 定義の再起動後の保存・トランザクション整合性（[定義と削除の公式 SQL](https://www.postgresql.org/docs/18/sql-createprocedure.html)） | ルーチンは `SQLEngine::new` 時に作るメモリ内レジストリ。新規定義の永続化/ロールバック経路は見当たらない。 | 未対応 |

## 2. PL/pgSQL 言語本文

PostgreSQL 18 は [`DECLARE ... BEGIN ... END`](https://www.postgresql.org/docs/18/plpgsql-structure.html) のブロック言語を定義する。現行レジストリは本文を保持しないため、以下の項目は固定手続き内部の Rust ロジックを除き、汎用 PL/pgSQL として実行できない。

| PostgreSQL 18 公式機能 | H2 Rust の現状と根拠 | 判定 |
| --- | --- | --- |
| [`DECLARE`、ローカル変数、`ALIAS`、`%TYPE` / `%ROWTYPE`、`RECORD`](https://www.postgresql.org/docs/18/plpgsql-declarations.html) | `ProcedureDef` に変数やブロック構造がなく、DDL 本文は保存されない。 | 未対応 |
| [代入と式評価](https://www.postgresql.org/docs/18/plpgsql-statements.html) | `:=` の PL/pgSQL 文を解析する経路なし。通常の SQL 式評価とは別。 | 未対応 |
| [`IF` / `CASE`、`LOOP` / `WHILE` / `FOR` / `FOREACH`、`EXIT` / `CONTINUE`](https://www.postgresql.org/docs/18/plpgsql-control-structures.html) | 本文の制御フローを実行する処理なし。 | 未対応 |
| [`PERFORM`、SQL 文の実行、`SELECT ... INTO [STRICT]`](https://www.postgresql.org/docs/18/plpgsql-statements.html) | 固定手続き内で Rust が SQL を直接発行するが、任意本文の SQL 文と変数束縛は未実装。 | 専用実装のみ |
| [動的 `EXECUTE ... USING` と結果取得](https://www.postgresql.org/docs/18/plpgsql-statements.html) | PL/pgSQL の動的 SQL パーサー・パラメータ束縛なし。 | 未対応 |
| [`FOUND`、`GET DIAGNOSTICS`、`ROW_COUNT`](https://www.postgresql.org/docs/18/plpgsql-statements.html) | ルーチン実行状態を保持する PL/pgSQL 変数・診断機構なし。 | 未対応 |
| [`RETURN`、`RETURN NEXT`、`RETURN QUERY`](https://www.postgresql.org/docs/18/plpgsql-control-structures.html) | 固定名ごとの Rust 返却のみ。ユーザー定義関数の戻り式や集合返却なし。 | 専用実装のみ |
| [`EXCEPTION` とブロック単位の部分ロールバック](https://www.postgresql.org/docs/18/plpgsql-control-structures.html) | PL/pgSQL 例外ブロック・副トランザクションなし。 | 未対応 |
| [`RAISE` / `ASSERT` と SQLSTATE](https://www.postgresql.org/docs/18/plpgsql-errors-and-messages.html) | 任意本文からの通知・例外・アサーション発行なし。 | 未対応 |
| [`refcursor`、`OPEN` / `FETCH` / `CLOSE`、カーソルループ](https://www.postgresql.org/docs/18/plpgsql-cursors.html) | トップレベル SQL の `DECLARE CURSOR` 等は別機能として存在するが、PL/pgSQL のカーソル変数・ループはない。 | 未対応 |
| [`DO` 匿名コードブロック](https://www.postgresql.org/docs/18/sql-do.html) | PL/pgSQL 本文を直接実行する分岐なし。 | 未対応 |
| [`NEW` / `OLD` / `TG_*` を使うトリガー関数](https://www.postgresql.org/docs/18/plpgsql-trigger.html) | `CREATE TRIGGER` と PL/pgSQL トリガー呼出し経路を確認できない。 | 未対応 |
| [手続き内の `COMMIT` / `ROLLBACK`](https://www.postgresql.org/docs/18/plpgsql-transactions.html) | 固定手続きは呼出元トランザクションで SQL を発行する。本文内のトランザクション制御と PostgreSQL の呼出し文脈制約はない。 | 未対応 |
| [PL/pgSQL の変数置換と計画キャッシュ](https://www.postgresql.org/docs/18/plpgsql-implementation.html) | 一般 SQL の AST キャッシュはあるが、PL/pgSQL 変数を SQL パラメータへ変換するコンパイラ/キャッシュはない。 | 未対応 |

## 3. 実行の正確性・権限で特に注意する差分

| 観点 | コードから確認できること | 影響 / 必要な確認 |
| --- | --- | --- |
| **成功の偽装** | 未知の `CALL` は成功値を返し、`DROP FUNCTION/PROCEDURE` も無条件に成功を返す。 | アプリケーションが業務処理完了や定義削除を誤認する。下の最小再現例で実測すべき。 |
| **権限の伝播** | 固定手続きは内部 SQL を `execute_with_user_and_tx(..., None)` で発行する。`SQLEngine` の文別権限確認は `user: Some(_)` の場合にだけ実行される。 | 呼出元の表権限を内部 SQL が引き継がない経路がある。`CREATE` / `CALL` 分岐自体も通常の文別権限確認より前にある。[PostgreSQL の `SECURITY INVOKER/DEFINER`](https://www.postgresql.org/docs/18/sql-createprocedure.html) に照らし、一般ユーザーから固定手続きを試す必要がある。 |
| **エラーの伝播** | 固定手続きの複数の内部 SQL は `let _ = ...` または `if let Ok(...)` でエラーを捨て、既定値を用いて結果行を作る。 | 一部更新だけを行って成功応答する可能性がある。制約違反・権限拒否・欠損表で原子性を確認すべき。 |
| **値と型** | 引数は SQL AST でなく文字列分割し、数値変換失敗時は `unwrap_or` の既定値へ置換する。結果には固定値も含む。 | 引数不一致や不正型を正しい入力として受け取る可能性がある。PostgreSQL の署名・型解決と異なる。 |
| **カタログの真実性** | `pg_proc` への SELECT は SQL 文字列の部分一致で固定行を返す。 | 作成・削除・再起動後のルーチン一覧や種類を反映しない。`\` コマンド等のクライアント検出にも影響する。 |

この表は実装を読んで特定した**経路とリスク**であり、権限昇格やデータ不整合が特定の入力で成立したと実行検証した結果ではない。

## 4. 差分を確かめる最小再現例

使い捨て DB で PostgreSQL 18 と H2 Rust の結果を比較する。期待結果は PostgreSQL 18 の仕様であり、H2 Rust の結果欄は実測して埋める。

| ケース | SQL / 操作 | PostgreSQL 18 の期待結果 | H2 Rust の確認点 |
| --- | --- | --- | --- |
| 任意本文 | `CREATE FUNCTION add_one(x int) RETURNS int AS $$ BEGIN RETURN x + 1; END $$ LANGUAGE plpgsql; SELECT add_one(4);` | `5` | DDL の成功だけでなく本文が評価されるか。 |
| 未定義手続き | `CALL does_not_exist();` | 未定義ルーチンのエラー | 成功応答を返さないか。 |
| 削除 | `CREATE PROCEDURE p() LANGUAGE plpgsql AS $$ BEGIN NULL; END $$; DROP PROCEDURE p(); CALL p();` | 最後の CALL はエラー | DROP が実際にレジストリを変更するか。 |
| ロールバック | `BEGIN; CREATE FUNCTION f() RETURNS int AS $$ BEGIN RETURN 1; END $$ LANGUAGE plpgsql; ROLLBACK; SELECT f();` | 最後の SELECT はエラー | 登録がトランザクションと連動するか。 |
| 権限 | 表更新権限のない一般ユーザーで、固定手続きが更新する表の行を読む前後に `CALL` | `SECURITY INVOKER` なら権限規則に従う | 内部 SQL が呼出元権限を飛ばさないか。 |
| カタログ | 独自関数の作成後、`SELECT proname, prokind FROM pg_proc WHERE proname='add_one'` | 登録に対応する `f` 行 | 固定 `p` 行を返さないか。 |

## 5. 実装の優先順位

1. **P0: 偽の成功をなくす。** 未知の `CALL`、未実装本文、`DROP` に明示的なエラーを返す。固定手続きでも内部 SQL のエラーと呼出元ユーザーを伝播し、更新全体を失敗時にロールバックする。
2. **P1: 互換性の境界を API とドキュメントで表示する。** `CREATE ... LANGUAGE plpgsql` の受付を「実行できる」と誤解させない。固定名と利用可能な引数を明記し、`pg_proc` の偽装をやめるか実際の登録状態に基づく最小カタログへ置き換える。
3. **P2: 汎用 PL/pgSQL を段階的に実装する場合。** 署名付き永続カタログ、ブロック/変数/式、静的 SQL と束縛、制御フロー、戻り値、例外、副トランザクション、カーソル、トリガーの順で、公式章の例を適合テストへ取り込む。

現行の HammerDB 疎通・性能記録は固定ルーチンによる成果として有効だが、PostgreSQL 18 の PL/pgSQL 全般の互換性を示す証拠にはならない。
