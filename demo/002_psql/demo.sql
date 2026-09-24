-- =====================================================================
-- H2 Database in Rust: psql クライアント用 デモスクリプト
-- =====================================================================

-- 1. サーバー稼働ノードの確認
SELECT id, hostname, ip_address, status, cpu_usage FROM server_nodes;

-- 2. JSON フィールドの検索（->> 演算子）
SELECT hostname, metadata ->> 'zone' AS zone, metadata ->> 'role' AS role
FROM server_nodes
WHERE metadata ->> 'role' = 'primary';

-- 3. 新規ノードの追加
INSERT INTO server_nodes VALUES 
(4, 'node-fukuoka-01', '192.168.3.10', 'ONLINE', 18.3, '{"zone": "ap-northeast-1d", "role": "edge"}');

-- 4. 集約と平均 CPU 使用率の計算
SELECT status, COUNT(*) AS count, AVG(cpu_usage) AS avg_cpu
FROM server_nodes
GROUP BY status;

-- 5. 明示的トランザクション
BEGIN;
UPDATE server_nodes SET status = 'MAINTENANCE' WHERE id = 3;
SELECT hostname, status FROM server_nodes WHERE id = 3;
COMMIT;

-- 6. INFORMATION_SCHEMA システムビューの照会
SELECT table_name FROM information_schema.tables;
SELECT column_name, data_type, is_nullable FROM information_schema.columns WHERE table_name = 'server_nodes';
