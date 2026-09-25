-- =====================================================================
-- H2 Database in Rust: 専用CLI 高度な SQL & クエリ演算 デモスクリプト
-- =====================================================================

-- ---------------------------------------------------------------------
-- 1. テーブルの定義 (DDL: 外部キー CASCADE 制約対応)
-- ---------------------------------------------------------------------
CREATE TABLE departments (
    id INT PRIMARY KEY,
    name VARCHAR(50) NOT NULL
);

CREATE TABLE employees (
    id INT PRIMARY KEY,
    name VARCHAR(100) NOT NULL,
    dept_id INT REFERENCES departments(id) ON DELETE CASCADE,
    salary DECIMAL(10, 2) NOT NULL,
    status VARCHAR(20) NOT NULL,
    profile JSON,
    notes TEXT
);

CREATE TABLE audit_log (
    log_id INT PRIMARY KEY,
    emp_name VARCHAR(100),
    action VARCHAR(50),
    recorded_at VARCHAR(50)
);

-- ---------------------------------------------------------------------
-- 2. Instant / Online DDL (テーブル定義の即時変更)
-- ---------------------------------------------------------------------
-- カラムの即時追加 (Instant ADD COLUMN)
ALTER TABLE employees ADD COLUMN bonus DECIMAL(10, 2) DEFAULT 0.00;

-- インデックスの並行作成 (CREATE INDEX CONCURRENTLY)
CREATE INDEX idx_emp_salary ON employees (salary);

-- カラム名の変更 (ALTER TABLE RENAME TO / RENAME COLUMN)
-- ---------------------------------------------------------------------
-- 3. データの挿入 & RETURNING 句 (INSERT ... RETURNING)
-- ---------------------------------------------------------------------
INSERT INTO departments VALUES (1, 'Engineering');
INSERT INTO departments VALUES (2, 'Product Management');
INSERT INTO departments VALUES (3, 'Sales & Growth');

-- RETURNING による挿入レコードの即時受取
INSERT INTO employees VALUES (1, 'Alice Smith', 1, 9500.00, 'ACTIVE', '{"skills": ["Rust", "Distributed Systems"], "tier": "Lead"}', 'データベースエンジンと分散MVCCの設計担当', 1500.00) RETURNING id, name, salary;
INSERT INTO employees VALUES (2, 'Bob Jones', 1, 7800.00, 'ACTIVE', '{"skills": ["PostgreSQL", "C++", "Rust"], "tier": "Senior"}', 'ストレージエンジンとページキャッシュ開発担当', 800.00);
INSERT INTO employees VALUES (3, 'Carol White', 2, 8200.00, 'ACTIVE', '{"skills": ["Product", "UI/UX"], "tier": "Senior"}', 'ユーザー体験と製品ロードマップ統括', 1000.00);
INSERT INTO employees VALUES (4, 'David Brown', 3, 6500.00, 'INACTIVE', '{"skills": ["Sales", "B2B"], "tier": "Junior"}', '法人開拓およびアカウントマネジメント', 500.00);
INSERT INTO employees VALUES (5, 'Eve Taylor', 1, 10500.00, 'ACTIVE', '{"skills": ["Rust", "Security", "Linux"], "tier": "Principal"}', '暗号化ストレージと認証基盤アーキテクチャ', 2000.00);

-- ---------------------------------------------------------------------
-- 4. 高度な DML: UPSERT (ON CONFLICT) & MERGE INTO & UPDATE/DELETE RETURNING
-- ---------------------------------------------------------------------
-- UPSERT: 既存レコードの競合時は給与とボーナスを更新
INSERT INTO employees (id, name, dept_id, salary, status, notes, bonus)
VALUES (4, 'David Brown', 3, 7200.00, 'ACTIVE', '法人開拓（再アクティブ化）', 700.00)
ON CONFLICT (id) DO UPDATE SET salary = EXCLUDED.salary, status = EXCLUDED.status, bonus = EXCLUDED.bonus;

-- UPDATE ... RETURNING
UPDATE employees SET salary = salary * 1.05 WHERE dept_id = 1 RETURNING id, name, salary AS new_salary;

-- ---------------------------------------------------------------------
-- 5. 複数パターンの結合 (JOIN: INNER, LEFT, RIGHT, FULL, CROSS, NATURAL, USING)
-- ---------------------------------------------------------------------
-- LEFT OUTER JOIN
SELECT e.name, d.name AS dept_name, e.salary
FROM employees e
LEFT JOIN departments d ON e.dept_id = d.id
ORDER BY e.salary DESC;

-- CROSS JOIN (デカルト積)
SELECT d.name AS dept, p.tier
FROM departments d
CROSS JOIN (SELECT 'Junior' AS tier UNION ALL SELECT 'Senior' AS tier) p
ORDER BY d.name, p.tier;

-- ---------------------------------------------------------------------
-- 6. 集合演算子 (UNION, UNION ALL, INTERSECT, EXCEPT)
-- ---------------------------------------------------------------------
-- INTERSECT: Engineering(1) と 給与8000以上の共通集合
SELECT name FROM employees WHERE dept_id = 1
INTERSECT
SELECT name FROM employees WHERE salary >= 8000.00;

-- EXCEPT: 給与7000以上だが Engineering 以外
SELECT name FROM employees WHERE salary >= 7000.00
EXCEPT
SELECT name FROM employees WHERE dept_id = 1;

-- ---------------------------------------------------------------------
-- 7. 共通テーブル式 (CTE) & 再帰クエリ (WITH RECURSIVE)
-- ---------------------------------------------------------------------
-- 非再帰 CTE による部門別集計
WITH DeptStats AS (
    SELECT dept_id, AVG(salary) AS avg_sal, MAX(salary) AS max_sal, COUNT(*) AS emp_cnt
    FROM employees
    GROUP BY dept_id
)
SELECT d.name, s.emp_cnt, s.avg_sal, s.max_sal
FROM departments d
JOIN DeptStats s ON d.id = s.dept_id
ORDER BY s.avg_sal DESC;

-- 再帰 CTE (WITH RECURSIVE) による連番およびツリー展開シミュレーション
WITH RECURSIVE Hierarchy AS (
    SELECT 1 AS level, 'CEO' AS title
    UNION ALL
    SELECT level + 1, 'Sub-Level ' || CAST(level + 1 AS VARCHAR)
    FROM Hierarchy
    WHERE level < 4
)
SELECT level, title FROM Hierarchy;

-- ---------------------------------------------------------------------
-- 8. ウィンドウ関数 (ROW_NUMBER, RANK, DENSE_RANK, LEAD, LAG, NTILE)
-- ---------------------------------------------------------------------
SELECT 
    name, 
    salary,
    dept_id,
    ROW_NUMBER() OVER (ORDER BY salary DESC) AS row_num,
    RANK() OVER (ORDER BY salary DESC) AS rank_pos,
    LAG(salary, 1) OVER (ORDER BY salary ASC) AS prev_lower_sal,
    LEAD(salary, 1) OVER (ORDER BY salary ASC) AS next_higher_sal,
    NTILE(2) OVER (ORDER BY salary DESC) AS salary_tier
FROM employees
ORDER BY salary DESC;

-- ---------------------------------------------------------------------
-- 9. 行値コンストラクタ ((a, b) IN (...))
-- ---------------------------------------------------------------------
SELECT id, name, dept_id, status
FROM employees
WHERE (dept_id, status) IN ((1, 'ACTIVE'), (2, 'ACTIVE'))
ORDER BY id ASC;

-- ---------------------------------------------------------------------
-- 10. 日本語全文検索 (FT_SEARCH & FT_SEARCH_MORPH)
-- ---------------------------------------------------------------------
-- 2-Gram 日本語全文検索
SELECT id, name, notes
FROM employees
WHERE FT_SEARCH(notes, 'アーキテクチャ');

-- 文字種境界形態素解析 全文検索
SELECT id, name, notes
FROM employees
WHERE FT_SEARCH_MORPH(notes, 'ストレージ 開発');

-- ---------------------------------------------------------------------
-- 11. JSON 演算子 (->, ->>)
-- ---------------------------------------------------------------------
SELECT name, profile -> 'skills' AS skills, profile ->> 'tier' AS tier
FROM employees
WHERE profile ->> 'tier' = 'Senior';

-- ---------------------------------------------------------------------
-- 12. 外部キー制約と CASCADE 削除の検証
-- ---------------------------------------------------------------------
-- 部門 3 (Sales & Growth) を削除 -> 所属社員 David Brown も CASCADE 自動削除
DELETE FROM departments WHERE id = 3;

-- 社員一覧確認 (David Brown が消えていること)
SELECT id, name, dept_id FROM employees ORDER BY id ASC;
