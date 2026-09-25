# Demo 012: 専用CLI デモ 2 - システム・拡張型・運用・カーソル (System & Ops Suite)

H2 Database Rust の**専用対話型 CLI（`h2-cli`）**を用いて、シーケンス生成器、自動採番型（SERIAL）、時間間隔型（INTERVAL）、サーバサイドカーソル、高度な数学・正規表現関数、CSV 一括移行、物理バックアップ＆リストア、および VACUUM を一括実演するデモです。

---

## 🎯 実演している主な機能

1. **シーケンス生成器 (SEQUENCE)**
   - `CREATE SEQUENCE invoice_seq INCREMENT BY 5 START WITH 1000`
   - `NEXTVAL('invoice_seq')`, `CURRVAL('invoice_seq')`
   - `SETVAL('invoice_seq', 5000)`: 任意値への直接設定
2. **自動採番型 (SERIAL) ＆ 時間計算 (INTERVAL)**
   - `id SERIAL PRIMARY KEY`: 自動採番カラム
   - `NOW() + INTERVAL '30 days'`: 日時と時間間隔（INTERVAL）の加算演算
3. **サーバサイドカーソル (CURSOR)**
   - `DECLARE inv_cursor CURSOR FOR SELECT ...`
   - `FETCH NEXT`, `FETCH PRIOR`, `FETCH FIRST`, `FETCH ABSOLUTE 3`: 前後・絶対位置指定フェッチ
   - `CLOSE inv_cursor`: リソース解放
4. **高度な数学・文字列・正規表現関数**
   - 自然対数/指数（`LN`, `EXP`）、常用対数（`LOG10`）、三角関数（`SIN`, `RADIANS`）、桁切り捨て（`TRUNC`）、符号判定（`SIGN`）
   - 文字列パディング（`LPAD`, `RPAD`）、反転（`REVERSE`）、文字置換（`TRANSLATE`）、頭文字大文字化（`INITCAP`）
   - 正規表現置換（`REGEXP_REPLACE`）、正規表現一致判定演算子（`~`, `~*`）
5. **実行計画 (EXPLAIN) ＆ スキーマ照会 (SHOW)**
   - `EXPLAIN SELECT ...`: フィルタおよびプロジェクション実行計画
   - `SHOW TABLES`, `SHOW COLUMNS FROM invoices`: テーブル・カラムメタデータ照会
6. **CSV 一括データ移行 (COPY TO / FROM)**
   - `COPY invoices TO 'target/invoices_export.csv' WITH (FORMAT CSV, HEADER)`
   - `COPY invoices_imported FROM 'target/invoices_export.csv' WITH (FORMAT CSV, HEADER)`
7. **バックアップ・リストア ＆ ストレージコンパクション**
   - `BACKUP TO 'target/h2_backup_demo.zip'`: 物理 ZIP アーカイブ保存
   - `RESTORE FROM 'target/h2_backup_demo.zip'`: 完全復元
   - `VACUUM`: ガベージ回収とファイル縮小

---

## 🚀 実行方法

**Windows:**
```cmd
run.bat
```
または
```cmd
cargo run -p h2-cli -- -f demo/012_cli_system_and_maintenance/script.sql
```

**Linux / macOS:**
```bash
chmod +x run.sh
./run.sh
```

**対話型シェル内からの実行:**
```bash
cargo run -p h2-cli
h2> .read demo/012_cli_system_and_maintenance/script.sql
```
