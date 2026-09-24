-- =====================================================================
-- H2 Database in Rust: 専用CLI用 サンプルスクリプト
-- =====================================================================

-- 1. テーブルの作成
CREATE TABLE IF NOT EXISTS departments (
    id INT PRIMARY KEY,
    name VARCHAR(50) NOT NULL
);

CREATE TABLE IF NOT EXISTS employees (
    id INT PRIMARY KEY,
    name VARCHAR(100) NOT NULL,
    dept_id INT,
    salary DECIMAL(10, 2) NOT NULL,
    profile JSON,
    notes TEXT
);

-- 2. セカンダリインデックスの作成
CREATE INDEX idx_emp_salary ON employees (salary);

-- 3. データの挿入
INSERT INTO departments VALUES (1, 'Engineering');
INSERT INTO departments VALUES (2, 'Product Design');
INSERT INTO departments VALUES (3, 'Sales & Marketing');

INSERT INTO employees VALUES (
    1, 'Alice Smith', 1, 8500.00,
    '{"skills": ["Rust", "Distributed Systems"], "remote": true}',
    'データベースエンジンのコアアーキテクチャ担当'
);

INSERT INTO employees VALUES (
    2, 'Bob Jones', 1, 7200.00,
    '{"skills": ["Rust", "PostgreSQL", "C++"], "remote": false}',
    'ストレージ層およびMVCCトランザクション開発'
);

INSERT INTO employees VALUES (
    3, 'Carol White', 2, 6800.00,
    '{"skills": ["Figma", "UI/UX"], "remote": true}',
    'デザインシステムとユーザー体験設計'
);

INSERT INTO employees VALUES (
    4, 'David Brown', 3, 7500.00,
    '{"skills": ["Sales", "Negotiation"], "remote": false}',
    'エンタープライズ顧客の導入支援'
);

-- 4. 基本クエリ & インデックススキャン (IndexScan)
SELECT id, name, salary FROM employees WHERE salary >= 7500.00 ORDER BY salary DESC;

-- 5. テーブル結合 (INNER JOIN)
SELECT e.name AS employee_name, d.name AS department_name, e.salary
FROM employees e
INNER JOIN departments d ON e.dept_id = d.id
ORDER BY e.id ASC;

-- 6. 集約関数 & グループ化 (GROUP BY)
SELECT d.name AS department, COUNT(*) AS count, AVG(e.salary) AS avg_salary
FROM employees e
INNER JOIN departments d ON e.dept_id = d.id
GROUP BY d.name
HAVING COUNT(*) >= 1
ORDER BY avg_salary DESC;

-- 7. 日本語全文検索 (N-Gram & 形態素解析)
SELECT name, notes FROM employees WHERE FT_SEARCH(notes, 'アーキテクチャ');
SELECT name, notes FROM employees WHERE FT_SEARCH_MORPH(notes, 'ストレージ トランザクション');

-- 8. JSON オブジェクトの抽出演算子 (->, ->>)
SELECT name, profile -> 'skills' AS skills, profile ->> 'remote' AS is_remote
FROM employees
WHERE profile ->> 'remote' = 'true';

-- 9. トランザクション (BEGIN / ROLLBACK / COMMIT)
BEGIN;
UPDATE employees SET salary = salary + 500.00 WHERE id = 1;
SELECT name, salary FROM employees WHERE id = 1;
ROLLBACK;

-- ロールバックされたため給与は元のまま
SELECT name, salary FROM employees WHERE id = 1;

-- 10. ストレージのコンパクション (Vacuum)
VACUUM;
