# demo/007: トランザクショナル・キューテーブル & JMS API (Native MQ)

本デモは、データベーストランザクションと完全に一体化した **トランザクショナル・キューテーブル（Transactional Queue Table）** および **JMS 2.0/3.0 準拠 API**、**Kafka 風オフセットシーク**を実演します。

---

## 🌟 実演内容

1. **SQL 透過性 (`CREATE QUEUE TABLE`)**:
   - 通常のテーブル定義構文に加え、`WITH (RETENTION_HOURS = 24, MAX_BYTES = 10485760)` で保持期間と容量上限を指定
   - `INSERT` によるエンキュー、`SELECT` によるデキュー
   - `UPDATE` / `DELETE` / 二次インデックス作成は不変コミットログ保護のため自動ガード
2. **JMS 2.0/3.0 準拠 API**:
   - `JmsConnectionFactory`, `create_session()`, `create_producer()`, `create_consumer()` による標準的エンタープライズメッセージング
3. **Kafka 風オフセットシーク**:
   - `seek_to_beginning()`（先頭への巻き戻し）
   - `seek(offset)`（特定オフセットへのジャンプ）
   - 複数コンシューマグループによる独立した読み取りカーソル
4. **Transactional Outbox パターンの完全解消**:
   - 同一 ACID トランザクション（`BEGIN` 〜 `COMMIT`）内で、業務テーブルの `UPDATE` とキューへの `INSERT` を原子的（Atomic）に確定
   - 別途 Kafka や RabbitMQ との Dual-Write 問題や分散トランザクション（2PC）を一切不要化

---

## 🏃 実行方法

```bash
cargo run -p demo-mq
```
