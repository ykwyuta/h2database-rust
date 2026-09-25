-- ==============================================================================
-- Demo 014: ユーザー認証・アクセス元制限・テーブル権限管理 (DCL & Security)
-- ==============================================================================

-- 1. 管理者によるベーステーブル作成とデータ投入
CREATE TABLE sales_data (
    id INT PRIMARY KEY,
    product VARCHAR(50),
    amount DECIMAL(10, 2),
    region VARCHAR(20)
);

CREATE TABLE customer_profiles (
    id INT PRIMARY KEY,
    name VARCHAR(50),
    email VARCHAR(100),
    phone VARCHAR(20)
);

CREATE TABLE executive_payroll (
    id INT PRIMARY KEY,
    employee_name VARCHAR(50),
    monthly_salary INT
);

INSERT INTO sales_data VALUES
    (1, 'Cloud Server Plan A', 150.00, 'US-East'),
    (2, 'Cloud Server Plan B', 300.00, 'AP-East'),
    (3, 'Dedicated Database', 800.00, 'EU-Central');

INSERT INTO customer_profiles VALUES
    (101, 'Acme Corp', 'contact@acme.com', '+1-555-0100'),
    (102, 'Beta Ltd', 'info@beta.com', '+44-20-7946');

INSERT INTO executive_payroll VALUES
    (1, 'CEO Alice', 25000),
    (2, 'CTO Bob', 22000);

-- 2. ユーザー作成 (CREATE USER: パスワード設定およびアクセス元制限)
-- analyst: ローカル接続 (localhost) のみ許可
CREATE USER 'analyst' PASSWORD 'analyst2026' HOST 'localhost';

-- operator: 社内サブネット (192.168.1.0/24) のみ許可
CREATE USER 'operator' PASSWORD 'op_secret' HOST '192.168.1.0/24';

-- remote_service: どこからでも接続可能 (%)
CREATE USER 'remote_service' PASSWORD 'srv_token' HOST '%';

-- 3. 登録ユーザー一覧の確認 (SHOW USERS)
SHOW USERS;

-- 4. テーブル単位の権限付与 (GRANT)
-- analyst には sales_data の SELECT 権限のみ付与（個人情報や給与テーブルは非公開）
GRANT SELECT ON TABLE sales_data TO 'analyst';

-- operator には sales_data の SELECT, INSERT, UPDATE 権限を付与
GRANT SELECT, INSERT, UPDATE ON TABLE sales_data TO 'operator';

-- remote_service には customer_profiles の SELECT 権限を付与
GRANT SELECT ON TABLE customer_profiles TO 'remote_service';

-- 5. ユーザーごとの権限一覧の確認 (SHOW GRANTS)
SHOW GRANTS FOR 'analyst';
SHOW GRANTS FOR 'operator';
SHOW GRANTS FOR 'remote_service';

-- 6. 権限剥奪 (REVOKE)
-- operator から UPDATE 権限を剥奪
REVOKE UPDATE ON TABLE sales_data FROM 'operator';
SHOW GRANTS FOR 'operator';

-- 7. ユーザー情報変更 (ALTER USER)
-- analyst のパスワード変更および接続許可ホストの全開放
ALTER USER 'analyst' PASSWORD 'new_analyst_2026' HOST '%';
SHOW USERS;

-- 8. ユーザー削除 (DROP USER)
DROP USER 'operator';
SHOW USERS;
