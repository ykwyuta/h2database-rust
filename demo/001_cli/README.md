# demo/001 専用CLI (Dedicated Interactive CLI)

本デモでは、`h2database-rust` に付属する軽量かつ高速な対話型シェル（REPL）である **`h2-cli`** の使い方を解説します。

---

## 🚀 クイックスタート

### 1. 起動方法

リポジトリルートから以下のコマンドを実行します。

```bash
# ファイル永続化モード（存在しない場合は自動作成）
cargo run -p h2-cli -- demo.h2

# またはインメモリ一時モード（引数なし）
cargo run -p h2-cli
```

Windows の場合は `run.bat`、Linux / macOS の場合は `run.sh` を直接実行することもできます。

---

## 💻 コマンドラインの操作方法

CLI が起動すると、プロンプト `h2>` が表示されます。

```text
Connected to h2 database at: demo.h2
Type SQL queries ending with ';' or '.exit' to quit.
h2> 
```

### 主なルール:
1. **セミコロン `;` で実行**:
   SQL 文はセミコロンを入力して Enter を押すまで複数行にまたがって記述可能です。
2. **終了コマンド**:
   `.exit` または `quit` を入力するか、`Ctrl + C` を押すと終了します。

---

## 📝 サンプル実行例

本ディレクトリ内の [`sample_queries.sql`](./sample_queries.sql) に記載されたクエリを順に入力して試すことができます。

### ① テーブル作成とインデックス
```sql
h2> CREATE TABLE employees (id INT PRIMARY KEY, name VARCHAR, salary DECIMAL(10, 2), notes TEXT);
Query executed in 1.4ms. (DDL)

h2> CREATE INDEX idx_emp_salary ON employees (salary);
Query executed in 0.9ms. (DDL)
```

### ② データの挿入
```sql
h2> INSERT INTO employees VALUES (1, 'Alice', 8500.00, 'コアアーキテクチャ担当');
Query executed in 0.5ms. (1 rows affected)

h2> INSERT INTO employees VALUES (2, 'Bob', 7200.00, 'ストレージ層開発');
Query executed in 0.4ms. (1 rows affected)
```

### ③ クエリと表形式表示
```sql
h2> SELECT id, name, salary FROM employees WHERE salary >= 7000.00 ORDER BY salary DESC;
+----+-------+---------+
| id | name  | salary  |
+----+-------+---------+
| 1  | Alice | 8500.00 |
| 2  | Bob   | 7200.00 |
+----+-------+---------+
2 rows in set (0.3ms).
```

### ④ 日本語全文検索 (FTS)
```sql
h2> SELECT name, notes FROM employees WHERE FT_SEARCH(notes, 'アーキテクチャ');
+-------+--------------------------------+
| name  | notes                          |
+-------+--------------------------------+
| Alice | コアアーキテクチャ担当         |
+-------+--------------------------------+
1 rows in set (0.6ms).
```

### ⑤ トランザクション (BEGIN / ROLLBACK)
```sql
h2> BEGIN;
Query executed in 0.1ms. (DDL)

h2> UPDATE employees SET salary = salary + 1000.00 WHERE id = 1;
Query executed in 0.4ms. (1 rows affected)

h2> ROLLBACK;
Query executed in 0.2ms. (DDL)

h2> SELECT name, salary FROM employees WHERE id = 1;
+-------+---------+
| name  | salary  |
+-------+---------+
| Alice | 8500.00 |
+-------+---------+
1 rows in set (0.2ms).
```

### ⑥ ストレージコンパクション (Vacuum)
```sql
h2> VACUUM;
Query executed in 1.1ms. (DDL)
```

---

## 🛠️ スクリプトの一括流し込み実行

Bash や PowerShell のパイプ機能を使って、SQL スクリプトを一括実行することも可能です。

```bash
# PowerShell
Get-Content sample_queries.sql | cargo run -p h2-cli -- demo.h2

# Linux / macOS Bash
cargo run -p h2-cli -- demo.h2 < sample_queries.sql
```
