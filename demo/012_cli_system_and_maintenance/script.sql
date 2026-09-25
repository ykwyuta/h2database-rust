-- =====================================================================
-- H2 Database in Rust: 専用CLI システム・拡張型・運用・カーソル デモスクリプト
-- =====================================================================

-- ---------------------------------------------------------------------
-- 1. シーケンス生成器 (SEQUENCE: CREATE, NEXTVAL, CURRVAL, SETVAL)
-- ---------------------------------------------------------------------
CREATE SEQUENCE invoice_seq INCREMENT BY 5 START WITH 1000;

SELECT NEXTVAL('invoice_seq') AS next_val_1;
SELECT NEXTVAL('invoice_seq') AS next_val_2;
SELECT CURRVAL('invoice_seq') AS current_val;

-- シーケンスの任意値への直接設定 (SETVAL)
SELECT SETVAL('invoice_seq', 5000) AS reset_val;
SELECT NEXTVAL('invoice_seq') AS after_reset_val;

-- ---------------------------------------------------------------------
-- 2. SERIAL 自動採番 & 拡張データ型 (UUID, INTERVAL, DECIMAL)
-- ---------------------------------------------------------------------
CREATE TABLE invoices (
    id SERIAL PRIMARY KEY,
    title VARCHAR(100) NOT NULL,
    amount DECIMAL(12, 2) NOT NULL,
    created_at VARCHAR(50) NOT NULL
);

INSERT INTO invoices (title, amount, created_at) VALUES ('Server Hosting Fee', 2500.00, '2026-09-01 10:00:00');
INSERT INTO invoices (title, amount, created_at) VALUES ('Domain Registration', 35.50, '2026-09-10 12:00:00');
INSERT INTO invoices (title, amount, created_at) VALUES ('Database License', 12000.00, '2026-09-15 15:30:00');
INSERT INTO invoices (title, amount, created_at) VALUES ('Consulting Services', 8500.00, '2026-09-20 09:00:00');

SELECT id, title, amount FROM invoices ORDER BY id ASC;

-- INTERVAL による期限日計算
SELECT title, amount, NOW() AS issue_date, NOW() + INTERVAL '30 days' AS payment_due
FROM invoices
ORDER BY id ASC;

-- ---------------------------------------------------------------------
-- 3. サーバサイドカーソル (DECLARE CURSOR, FETCH, CLOSE)
-- ---------------------------------------------------------------------
DECLARE inv_cursor CURSOR FOR
    SELECT id, title, amount FROM invoices ORDER BY id ASC;

-- 順次フェッチ (NEXT)
FETCH NEXT FROM inv_cursor;
FETCH NEXT FROM inv_cursor;

-- 逆方向フェッチ (PRIOR)
FETCH PRIOR FROM inv_cursor;

-- 先頭行へジャンプ (FIRST)
FETCH FIRST FROM inv_cursor;

-- 絶対位置へジャンプ (ABSOLUTE)
FETCH ABSOLUTE 3 FROM inv_cursor;

-- カーソルの解放
CLOSE inv_cursor;

-- ---------------------------------------------------------------------
-- 4. 高度な数学・文字列・正規表現関数
-- ---------------------------------------------------------------------
-- 数学関数: 対数、指数、三角関数、桁切り捨て、符号
SELECT 
    LN(EXP(2.0)) AS natural_log_exp,
    LOG10(1000.0) AS log10_val,
    SIN(RADIANS(90.0)) AS sin_90_deg,
    TRUNC(123.4567, 2) AS truncated_val,
    SIGN(-99.5) AS sign_negative;

-- 文字列関数: パディング、反転、文字置換、単語頭大文字化
SELECT 
    LPAD('123', 6, '0') AS padded_id,
    RPAD('Rust', 8, '!') AS padded_right,
    REVERSE('Database') AS reversed_str,
    TRANSLATE('2026/09/25', '/', '-') AS translated_date,
    INITCAP('high performance rdbms in rust') AS capitalized_title;

-- 正規表現演算子 (~, ~*) & 置換関数 (REGEXP_REPLACE)
SELECT 
    REGEXP_REPLACE('Order-98765-ABC', '[0-9]+', 'XXXXX') AS masked_order,
    'engineer@h2database.com' ~* '^[A-Z0-9._%+-]+@[A-Z0-9.-]+\.[A-Z]{2,}$' AS is_valid_email;

-- ---------------------------------------------------------------------
-- 5. 実行計画の確認 (EXPLAIN) & メタデータ照会 (SHOW)
-- ---------------------------------------------------------------------
EXPLAIN SELECT * FROM invoices WHERE amount > 5000.00;

SHOW TABLES;

SHOW COLUMNS FROM invoices;

-- ---------------------------------------------------------------------
-- 6. CSV データのエクスポート & インポート (COPY TO / FROM)
-- ---------------------------------------------------------------------
-- CSV へエクスポート
COPY invoices TO 'target/invoices_export.csv' WITH (FORMAT CSV, HEADER);

-- 受入用テーブルの作成
CREATE TABLE invoices_imported (
    id INT PRIMARY KEY,
    title VARCHAR(100),
    amount DECIMAL(12, 2),
    created_at VARCHAR(50)
);

-- CSV からインポート
COPY invoices_imported FROM 'target/invoices_export.csv' WITH (FORMAT CSV, HEADER);

-- インポート確認
SELECT id, title, amount FROM invoices_imported ORDER BY id ASC;

-- ---------------------------------------------------------------------
-- 7. バックアップとリストア (BACKUP / RESTORE) & ストレージ縮小 (VACUUM)
-- ---------------------------------------------------------------------
BACKUP TO 'target/h2_backup_demo.zip';

RESTORE FROM 'target/h2_backup_demo.zip';

VACUUM;
