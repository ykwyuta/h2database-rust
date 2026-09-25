# Demo 013: 専用CLI デモ 3 - トランザクショナル・キューテーブル (Native MQ Suite)

H2 Database Rust の**専用対話型 CLI（`h2-cli`）**を用いて、RDBMS ネイティブなメッセージキュー機能である**トランザクショナル・キューテーブル（`CREATE QUEUE TABLE`）**、SQL による直接エンキュー／内部オフセット走査、Kafka 風シーク再生、およびトランザクション・ロールバック連動（Transactional Outbox パターン不要の実証）を一括実演するデモです。

---

## 🎯 実演している主な機能

1. **トランザクショナル・キューテーブルの作成 (`CREATE QUEUE TABLE`)**
   - `WITH (RETENTION_HOURS = 48, MAX_BYTES = 10485760)` による保持期間＆容量上限の二重安全ガード
2. **アトミック・コミット実証 (`BEGIN ... COMMIT`)**
   - 業務テーブル（`user_notifications`）への更新とキューへのイベント投入が同一トランザクション内で確実にコミットされます。
3. **アトミック・ロールバック実証 (Outbox 不要の実証)**
   - 途中で `ROLLBACK` を実行した場合、業務テーブルの変更だけでなく、**キューテーブルへ準備されたメッセージも完全に取り消され、一切残りません**。
   - 外部メッセージブローカー（RabbitMQ, Kafka等）で必要だった複雑な Outbox テーブルや CDC（Change Data Capture）プロセスが不要になります。
4. **Kafka 風の内部オフセット走査 (Seek / Replay / Stream)**
   - メッセージは受信によって破棄されず、シーケンシャルな内部オフセット（`_offset`）で追跡されます。
   - `WHERE _offset >= 0`: 過去の全メッセージを巻き戻して再処理（Replay）
   - `WHERE _offset >= 2`: 途中からのストリーム再開
   - `WHERE _offset >= 4`: 最新メッセージのみの購読
5. **キューテーブルの堅牢な安全ガード**
   - キューの整合性を守るため、`UPDATE`, `DELETE`, および `CREATE INDEX` の直接実行はエンジンによって安全にブロックされます。

---

## 🚀 実行方法

**Windows:**
```cmd
run.bat
```
または
```cmd
cargo run -p h2-cli -- -f demo/013_cli_transactional_mq/script.sql
```

**Linux / macOS:**
```bash
chmod +x run.sh
./run.sh
```

**対話型シェル内からの実行:**
```bash
cargo run -p h2-cli
h2> .read demo/013_cli_transactional_mq/script.sql
```
