-- =====================================================================
-- H2 Database in Rust: 専用CLI トランザクショナル・キューテーブル デモスクリプト
-- =====================================================================

-- ---------------------------------------------------------------------
-- 1. キューテーブルおよび業務テーブルの作成
-- ---------------------------------------------------------------------
-- 二重保持ポリシー付きトランザクショナル・キューテーブルの作成
CREATE QUEUE TABLE notification_queue (
    payload VARCHAR NOT NULL
) WITH (RETENTION_HOURS = 48, MAX_BYTES = 10485760);

-- 通常の業務テーブル
CREATE TABLE user_notifications (
    id INT PRIMARY KEY,
    user_name VARCHAR(50) NOT NULL,
    status VARCHAR(20) NOT NULL
);

-- ---------------------------------------------------------------------
-- 2. トランザクションによるアトミック・コミット実証
-- ---------------------------------------------------------------------
BEGIN;

INSERT INTO user_notifications VALUES (1, 'Alice Smith', 'PROCESSED');
INSERT INTO notification_queue (payload) VALUES ('{"event": "WELCOME_EMAIL", "userId": 1, "recipient": "alice@example.com"}');

COMMIT;

-- コミットの反映確認 (業務テーブルとキューの双方に格納)
SELECT id, user_name, status FROM user_notifications WHERE id = 1;

SELECT _offset, _timestamp, _msg_id, payload 
FROM notification_queue 
WHERE _offset >= 0 
ORDER BY _offset ASC;

-- ---------------------------------------------------------------------
-- 3. トランザクションによるアトミック・ロールバック実証 (Outbox 不要の実証)
-- ---------------------------------------------------------------------
BEGIN;

INSERT INTO user_notifications VALUES (999, 'Failed User', 'PENDING');
INSERT INTO notification_queue (payload) VALUES ('{"event": "SHOULD_BE_ROLLED_BACK", "userId": 999}');

-- トランザクションを破棄 (ロールバック)
ROLLBACK;

-- ロールバックの反映確認 (業務テーブルもキューも取り消され、ID 999 のメッセージは一切残らない)
SELECT id, user_name, status FROM user_notifications WHERE id = 999;

SELECT _offset, _msg_id, payload 
FROM notification_queue 
WHERE _offset >= 0 
ORDER BY _offset ASC;

-- ---------------------------------------------------------------------
-- 4. 複数イベントのエンキュー
-- ---------------------------------------------------------------------
INSERT INTO notification_queue (payload) VALUES ('{"event": "ORDER_SHIPPED", "userId": 1, "orderId": 501}');
INSERT INTO notification_queue (payload) VALUES ('{"event": "PAYMENT_RECEIVED", "userId": 1, "amount": 12500.00}');
INSERT INTO notification_queue (payload) VALUES ('{"event": "PASSWORD_CHANGED", "userId": 1, "device": "macOS"}');

-- ---------------------------------------------------------------------
-- 5. Kafka 風の内部オフセット走査 (Seek / Replay / Stream)
-- ---------------------------------------------------------------------
-- 先頭からの全件再生 (Rewind to Beginning: _offset >= 0)
SELECT _offset, _msg_id, payload 
FROM notification_queue 
WHERE _offset >= 0 
ORDER BY _offset ASC;

-- 特定オフセットからのシーク購読 (Seek to Offset >= 2)
SELECT _offset, _msg_id, payload 
FROM notification_queue 
WHERE _offset >= 2 
ORDER BY _offset ASC;

-- 最新オフセットのみの購読 (Seek to Latest: _offset >= 4)
SELECT _offset, _msg_id, payload 
FROM notification_queue 
WHERE _offset >= 4 
ORDER BY _offset ASC;

-- ---------------------------------------------------------------------
-- 6. キューテーブルの安全ガード (UPDATE / DELETE / INDEX 制限)
-- ---------------------------------------------------------------------
-- UPDATE の試行 (安全に拒否されること)
UPDATE notification_queue SET payload = 'corrupted';

-- DELETE の試行 (安全に拒否されること)
DELETE FROM notification_queue WHERE payload LIKE '%';

-- インデックス作成の試行 (安全に拒否されること)
CREATE INDEX idx_queue_payload ON notification_queue (payload);
