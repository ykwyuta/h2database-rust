# PostgreSQL 18 PL/pgSQL 汎用言語互換および SQL Server T-SQL 拡張設計方針書

## 1. 概要と背景課題

### 1.1 背景と現行の課題
[`docs/review/plpgsql/README.md`](file:///d:/workspace/h2database-rust/docs/review/plpgsql/README.md) にて指摘されたとおり、従来の `h2database-rust` における「PL/pgSQL」実装（[`crates/h2-sql/src/plpgsql_sim.rs`](file:///d:/workspace/h2database-rust/crates/h2-sql/src/plpgsql_sim.rs)）は、HammerDB の TPROC-C ベンチマークで用いられる 5 大ストアドプロシージャ（`neword`, `payment`, `delivery`, `ostat`, `slev`）および `dbms_random` を高速に実行するための**専用シミュレーション層**にとどまっていました。

これにより、以下の重大な課題・乖離が存在していました：
1. **成功の偽装 (P0 課題)**:
   - 未知のプロシージャに対する `CALL unknown_proc()` や `DROP PROCEDURE / DROP FUNCTION` が、存在確認を行わず無条件に `Ok(ExecutionResult::Ddl)` を返却し、アプリケーションに正常完了と誤認させていた。
2. **カタログの偽装 (P1 課題)**:
   - `pg_proc` への問い合わせに対して、固定値（`prokind='p'`, `cnt=5`）の 1 行を返すハードコード分岐となっており、定義された関数のメタデータ・引数・戻り値・関数種別が反映されていなかった。
3. **汎用 PL/pgSQL 言語本文の未実装 (P2 課題)**:
   - `DECLARE ... BEGIN ... END;` のブロック構文、ローカル変数宣言と初期化、代入演算子（`:=` / `=`）、条件分岐（`IF / ELSIF / ELSE`）、反復（`LOOP / WHILE / FOR`）、ループ制御（`EXIT / CONTINUE`）、戻り値（`RETURN expr`）、例外送出（`RAISE EXCEPTION / NOTICE`）、変数束縛を伴う問合せ（`SELECT ... INTO` / `PERFORM`）、SQL 文中のスカラー関数呼び出し（`SELECT func(x)`）が実装されていなかった。
4. **将来の SQL Server T-SQL (Transact-SQL) 互換性拡張への配慮**:
   - PL/pgSQL 構文に強く結合した実装にしてしまうと、今後予定されている SQL Server T-SQL（`@変数`, `SET @x = ...`, `IF ... BEGIN ... END ELSE BEGIN ... END`, `WHILE ... BEGIN ... END`, `RAISERROR / THROW` 等）の互換層を追加する際に二重実装やアーキテクチャの破綻を招く。

本設計書では、**共通手続き型実行エンジン（Canonical Procedural AST ＆ Runtime Interpreter）** を中核に据え、**PL/pgSQL フロントエンド構文解析器** と **将来の T-SQL フロントエンド構文解析器** をプラグ可能とするマルチダイアレクト手続き型アーキテクチャを定義し、PostgreSQL 18 PL/pgSQL 互換機能を完全実装します。

---

## 2. アーキテクチャ全体像

```
┌───────────────────────────────────────────────────────────────────────────────────┐
│                               SQL Language Frontends                              │
│                                                                                   │
│   ┌─────────────────────────────────────┐   ┌─────────────────────────────────┐   │
│   │ PL/pgSQL Dialect Parser             │   │ T-SQL Dialect Parser (Future)   │   │
│   │ - DECLARE x INT := 10;              │   │ - DECLARE @x INT = 10;          │   │
│   │ - IF cond THEN ... END IF;          │   │ - IF cond BEGIN ... END         │   │
│   │ - SELECT col INTO x FROM ...        │   │ - SELECT @x = col FROM ...      │   │
│   │ - RAISE EXCEPTION 'err';            │   │ - THROW 50000, 'err', 1;        │   │
│   └──────────────────┬──────────────────┘   └────────────────┬────────────────┘   │
└──────────────────────┼───────────────────────────────────────┼────────────────────┘
                       │                                       │
                       ▼                                       ▼
┌───────────────────────────────────────────────────────────────────────────────────┐
│                    Canonical Procedural AST (ProcAst)                             │
│                                                                                   │
│   - ProcBlock (label, declarations, body, exception_handlers)                     │
│   - VarDecl (name, data_type, default_expr, not_null)                             │
│   - ProcStmt:                                                                     │
│     * Assign (target, expr)                                                       │
│     * If (branches: Vec<(cond, stmts)>, else_branch)                              │
│     * While (cond, body, label)                                                   │
│     * ForRange (var, start, end, step, reverse, body)                             │
│     * Loop (body, label) / Exit (cond, label) / Continue (cond, label)            │
│     * Return (expr) / ReturnNext / ReturnQuery                                    │
│     * Raise (level, message, params)                                              │
│     * SelectInto (targets, query, strict) / Perform (expr)                        │
│     * ExecuteDynamic (sql_expr, into, using)                                      │
│     * SqlStmt (raw_sql with variable binding)                                     │
│   - ProcExpr (Literal, Variable, BinaryOp, UnaryOp, FuncCall, IsNull)             │
└──────────────────────────────────────┬────────────────────────────────────────────┘
                                       │
                                       ▼
┌───────────────────────────────────────────────────────────────────────────────────┐
│                  Common Procedural Runtime Interpreter                            │
│                                                                                   │
│   - ProcEnvironment / ProcFrame Stack (Lexical variable scopes, Coercion)        │
│   - Expression Evaluator (Arithmetic, Logic, Builtins, Nested UDFs)               │
│   - Variable Binder for Embedded SQL (Substitutes $var into parameters)           │
│   - Transaction & User Context Propagation (SECURITY INVOKER / DEFINER)           │
│   - Flow Control Evaluator (Return, Exit, Continue, Exception unwinding)          │
└──────────────────┬───────────────────────────────────┬────────────────────────────┘
                   │                                   │
                   ▼                                   ▼
┌──────────────────────────────────────┐   ┌────────────────────────────────────────┐
│ Routine Registry & Catalog           │   │ SQLEngine Execution Dispatcher         │
│                                      │   │                                        │
│ - Persisted in MVStore `_catalog`    │   │ - Scalar UDF evaluation in SELECT/expr │
│ - RoutineDef (kind, signature, body) │   │ - CALL routine(args) execution         │
│ - Virtual table `pg_proc` reflection │   │ - DROP FUNCTION/PROCEDURE check        │
└──────────────────────────────────────┘   └────────────────────────────────────────┘
```

---

## 3. T-SQL (SQL Server) 互換性拡張マッピング

将来の SQL Server T-SQL サポートを見据え、構文と AST の対応関係を以下のように設計します。共通 AST および実行エンジンには言語固有の依存を持たせず、ダイアレクトフロントエンドが正規化を行います。

| 概念 / 構文 | PostgreSQL PL/pgSQL | SQL Server T-SQL (将来拡張) | Canonical ProcAst 表現 |
| :--- | :--- | :--- | :--- |
| **ルーチン定義** | `CREATE FUNCTION f() RETURNS int AS $$ ... $$ LANGUAGE plpgsql;` | `CREATE FUNCTION f() RETURNS int AS BEGIN ... END` | `RoutineDef { kind: Function, lang, body: ProcBlock }` |
| **プロシージャ** | `CREATE PROCEDURE p() LANGUAGE plpgsql AS $$ ... $$;` | `CREATE PROCEDURE p AS BEGIN ... END` | `RoutineDef { kind: Procedure, lang, body: ProcBlock }` |
| **変数宣言** | `DECLARE x INT := 10;` | `DECLARE @x INT = 10;` | `VarDecl { name: "x", data_type: Integer, default: Some(10) }` |
| **代入文** | `x := x + 1;` または `x = x + 1;` | `SET @x = @x + 1;` | `ProcStmt::Assign { target: "x", expr: ... }` |
| **条件分岐** | `IF c THEN ... ELSIF c2 THEN ... ELSE ... END IF;` | `IF c BEGIN ... END ELSE IF c2 BEGIN ... END ELSE BEGIN ... END` | `ProcStmt::If { branches: [(c, ...), (c2, ...)], else_branch }` |
| **ループ** | `WHILE c LOOP ... END LOOP;` | `WHILE c BEGIN ... END` | `ProcStmt::While { condition: c, body: ... }` |
| **範囲ループ** | `FOR i IN 1..10 LOOP ... END LOOP;` | `SET @i=1; WHILE @i<=10 BEGIN ... SET @i=@i+1 END` | `ProcStmt::ForRange { var: "i", start: 1, end: 10, ... }` |
| **脱出・継続** | `EXIT WHEN c;` / `CONTINUE WHEN c;` | `IF c BREAK;` / `IF c CONTINUE;` | `ProcStmt::Exit { condition: Some(c) }` / `Continue` |
| **問合せ代入** | `SELECT col INTO x FROM tbl WHERE id = 1;` | `SELECT @x = col FROM tbl WHERE id = 1;` | `ProcStmt::SelectInto { targets: ["x"], query: ... }` |
| **値返却** | `RETURN x;` | `RETURN @x;` | `ProcStmt::Return { value: Some(x) }` |
| **エラー送出** | `RAISE EXCEPTION 'msg';` | `THROW 50000, 'msg', 1;` / `RAISERROR('msg', 16, 1);` | `ProcStmt::Raise { level: Exception, message: "msg" }` |
| **メッセージ** | `RAISE NOTICE 'msg';` | `PRINT 'msg';` | `ProcStmt::Raise { level: Notice, message: "msg" }` |
| **呼出し** | `CALL p(1, 2);` | `EXEC p 1, 2;` または `EXEC p @p1=1;` | `CallStatement` |

---

## 4. PL/pgSQL 言語仕様と文法

### 4.1 DDL 構文
```sql
CREATE [OR REPLACE] FUNCTION routine_name (
    [ [ argmode ] [ argname ] argtype [ { DEFAULT | = } default_expr ] [, ...] ]
)
RETURNS rettype
[ LANGUAGE plpgsql ]
[ { IMMUTABLE | STABLE | VOLATILE } ]
[ { CALLED ON NULL INPUT | RETURNS NULL ON NULL INPUT | STRICT } ]
[ { [ GLOBAL | LOCAL ] TEMPORARY | TEMP } ]
[ [ EXTERNAL ] SECURITY { DEFINER | INVOKER } ]
AS $$
[ DECLARE
    declarations ]
BEGIN
    statements
[ EXCEPTION
    WHEN condition THEN
        handler_statements ]
END;
$$;
```

```sql
CREATE [OR REPLACE] PROCEDURE routine_name (
    [ [ argmode ] [ argname ] argtype [ { DEFAULT | = } default_expr ] [, ...] ]
)
[ LANGUAGE plpgsql ]
[ [ EXTERNAL ] SECURITY { DEFINER | INVOKER } ]
AS $$
[ DECLARE
    declarations ]
BEGIN
    statements
END;
$$;
```

```sql
DO $$
[ DECLARE
    declarations ]
BEGIN
    statements
END;
$$ [ LANGUAGE plpgsql ];
```

### 4.2 変数宣言 (`DECLARE`)
- ローカル変数: `var_name [CONSTANT] type [NOT NULL] [ { := | = | DEFAULT } expr ];`
- ポジショナル引数エイリアス: `var_name ALIAS FOR $1;`

### 4.3 文（Statements）と制御フロー
1. **代入 (`:=` または `=`)**:
   `var := expr;`
2. **条件分岐 (`IF`)**:
   `IF boolean_expr THEN stmts [ELSIF boolean_expr THEN stmts ...] [ELSE stmts] END IF;`
3. **ループ構文**:
   - `LOOP stmts END LOOP;`
   - `WHILE boolean_expr LOOP stmts END LOOP;`
   - `FOR var IN [REVERSE] lower..upper [BY step] LOOP stmts END LOOP;`
   - `EXIT [label] [WHEN boolean_expr];`
   - `CONTINUE [label] [WHEN boolean_expr];`
4. **戻り値 (`RETURN`)**:
   - `RETURN expr;` (スカラー関数からの返却)
   - `RETURN;` (void 関数またはプロシージャからの即時脱出)
5. **問合せと変数格納**:
   - `SELECT expr1, expr2 INTO [STRICT] var1, var2 FROM ...;`
   - 単一行取得時は変数へ格納。`STRICT` 指定時に 0 行または 2 行以上の場合は例外送出。
   - `PERFORM query_or_expr;` (結果行を破棄して実行のみ行う)
6. **動的 SQL (`EXECUTE ... USING ...`)**:
   - `EXECUTE sql_expr [INTO var1, var2] [USING param1, param2];`
7. **例外とメッセージ (`RAISE`)**:
   - `RAISE [EXCEPTION | WARNING | NOTICE | INFO] 'format_string' [, expr, ...];`
   - `RAISE EXCEPTION` は現在のトランザクション/ブロックをアボート。
8. **埋め込み SQL 文**:
   - `INSERT INTO ...`, `UPDATE ...`, `DELETE ...`, `CREATE TABLE ...` 等の任意の DML/DDL。
   - スコープ内の変数は、SQL 文中の識別子またはパラメータとして自動的にバインド・解決される。

---

## 5. 実行時ランタイムインタプリタ (`ProcInterpreter`)

### 5.1 スコープとフレーム管理 (`ProcFrame`)
- 実行コンテキストはスタック形式の `ProcFrame` で管理。
- 入れ子ブロック（Nested `BEGIN ... END;`）や `FOR` ループでは子フレームを push し、ブロック終了時に pop。
- 変数探索は最深フレームから親フレームへレキシカルに解決。
- 型強制（Coercion）: 変数宣言時の `DataType` に合わせて、代入される `Value` を安全に型変換。

### 5.2 SQL 文への変数バインディング
PL/pgSQL 内で実行される SQL 文（例: `UPDATE accounts SET balance = balance - amt WHERE id = acc_id;`）において:
1. 現在のスコープ内に存在する変数名（`amt`, `acc_id` など）を検出。
2. 変数の現在値（リテラル化またはパラメータ化）に安全に置換。
3. 呼び出し元のトランザクション `tx` および実行ユーザー権限 `user` をそのまま引き継いで `SQLEngine::execute_with_user_and_tx` を実行。

### 5.3 権限とトランザクション境界
- `SECURITY INVOKER`（デフォルト）: 呼び出し元クライアントのユーザー権限で内部 SQL を実行。
- `SECURITY DEFINER`: ルーチンの所有者（定義者）の権限で内部 SQL を実行。
- トランザクション境界: 呼び出し元が `BEGIN` 中であれば同一トランザクション内で実行され、`RAISE EXCEPTION` や実行時エラーが発生した場合は自動的に全体がロールバックされる。

---

## 6. カタログ連携・システムビュー (`pg_proc`)

### 6.1 永続化カタログ
- `Catalog` に `routines: Arc<RwLock<HashMap<String, RoutineDef>>>` を保持。
- MVStore の `_catalog` マップに `proc:<routine_name>` キーで JSON 永続化。
- データベース再起動時にも `Catalog::new` で自動復元。

### 6.2 `pg_proc` 仮想カタログビュー
`executor.rs` において、`pg_proc` / `pg_catalog.pg_proc` への問い合わせを正規の仮想テーブルとして解決：
- **提供カラム**:
  - `proname` (VARCHAR): ルーチン名
  - `prokind` (CHAR): `'f'` (Function) または `'p'` (Procedure)
  - `prorettype` (VARCHAR): 戻り値の型名
  - `pronargs` (INTEGER): 引数の個数
  - `proargnames` (VARCHAR): 引数名のカンマ区切りまたは配列文字列表現
  - `prosrc` (VARCHAR): ルーチンのソースコード本文
- HammerDB などのクライアントが行う `SELECT p.prokind, count(*) AS cnt FROM pg_proc p GROUP BY ...` も、全登録ルーチンに基づく正確な集計結果を返却。

### 6.3 厳格な DDL / DML エラーハンドリング (P0 課題の解消)
- `CALL does_not_exist()`: カタログに存在しない場合は `H2Error::Execution("Procedure 'does_not_exist' does not exist")` を返却（成功偽装の完全撤廃）。
- `DROP PROCEDURE / DROP FUNCTION name`:
  - 対象が存在しない場合、`IF EXISTS` がなければエラーを返却。
  - 存在する場合はカタログから削除し、永続化ストアからも削除。
- 式評価におけるユーザー定義関数の呼び出し:
  - `SELECT add_one(4)`: カタログから `add_one` を検索し、引数 `4` を渡して `ProcInterpreter` で実行し、結果 `5` を返却。
  - 存在しない関数は `Unsupported scalar function / Function not found` エラー。

---

## 7. 実装・検証計画

1. **モジュール構成 (`crates/h2-sql/src/procedural/`)**:
   - `ast.rs`: 共通手続き型 AST 定義
   - `plpgsql/mod.rs` & `plpgsql/parser.rs`: PL/pgSQL 構文解析器
   - `tsql/mod.rs`: 将来の T-SQL 用モジュール設計
   - `interpreter.rs`: 共通手続き型ランタイムインタプリタ
   - `mod.rs`: `ProceduralEngine` および `RoutineDef` 定義
2. **カタログおよび実行層の統合**:
   - `crates/h2-sql/src/catalog.rs`: ルーチンの保存・永続化・削除
   - `crates/h2-sql/src/executor.rs`: `pg_proc` 仮想表、`CALL`、`DROP`、スカラー関数実行のディスパッチ
   - `crates/h2-sql/src/expression.rs`: スカラー式でのユーザー定義関数評価
3. **総合テスト (`crates/h2/tests/plpgsql_engine_tests.rs`)**:
   - 四則演算・戻り値スカラー関数 (`add_one`)
   - 条件分岐 (`IF ... ELSIF ... ELSE ... END IF`)
   - ループ構造 (`WHILE`, `FOR ... IN`, `LOOP`, `EXIT WHEN`)
   - テーブル問合せと変数代入 (`SELECT ... INTO`)
   - 未定義呼出しエラーおよび `DROP` の厳格な動作
   - `RAISE EXCEPTION` によるトランザクションロールバック
   - カタログ `pg_proc` からの動的取得と HammerDB TPROC-C 互換性の維持

## 7. HammerDB 暫定シミュレータ層の完全撤廃と汎用化の検証

従来の暫定実装であった `crates/h2-sql/src/plpgsql_sim.rs`（`PlPgSqlSimulator`）およびそれに付随するハードコード分岐をすべて削除しました：

1. **削除された暫定機能**:
   - `PlPgSqlSimulator` 構造体および Xorshift64 による固定擬似乱数生成器
   - `sim_neword`, `sim_payment`, `sim_delivery`, `sim_ostat`, `sim_slev` などの Rust ネイティブ直接シミュレーションコード
   - `SQLEngine::execute_select` における `SELECT DBMS_RANDOM` や `SELECT neword(...)` へのハードコード文字列一致インターセプト
   - `resolve_pg_proc_table` 内でハードコードされていた 6 件の固定ルーチン一覧（`builtins`）
   - 未知プロシージャに対するダミーの成功応答（`Ok(ExecutionResult::Ddl)`）
2. **汎用エンジンによる代替・不要化**:
   - `CREATE OR REPLACE FUNCTION DBMS_RANDOM (INTEGER, INTEGER) ...` は `parse_create_routine` により正式に構文解析され、カタログに登録される。
   - `DBMS_RANDOM` の本文に含まれる `DECLARE ... ALIAS FOR ... BEGIN RETURN trunc(random() * ...) END;` は、組み込みの `random()` および `trunc()` 関数と変数スコープを介して `ProcInterpreter` で完全に評価される。
   - `NEWORD` / `PAYMENT` 等のプロシージャも、`IN` / `INOUT` 引数を正しく解釈し、手続き本文を実行した上で更新された `INOUT` 値を行セットとしてクライアントへ返却する。
   - `pg_proc` はカタログの全登録ルーチンのみを動的に返却するため、クライアントによるスキーマ検査（`prokind = 'p'` / `'f'` の件数カウント等）も実カタログに基づき整合して動作する。
3. **検証結果**:
   - `test_hammerdb_provisional_simulator_obsolete_and_functional` を含む全テストが成功し、暫定層なしで HammerDB ワークロードの全シーケンスが実行可能であることを確認。

