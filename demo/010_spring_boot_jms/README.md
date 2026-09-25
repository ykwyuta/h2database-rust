# Demo 010: Spring Boot 4.1 / Spring JMS Standard & MyBatis Demo

H2 Database Rust の**トランザクショナル・キューテーブル（`CREATE QUEUE TABLE`）**を、Java の標準エンタープライズフレームワークである **Spring Boot 4.1（Java 21 / Spring Framework 6.2+）**、**Spring JMS スタンダード（`JmsTemplate`, `@JmsListener`, `ConnectionFactory`）**、および **MyBatis（XML マッパー）** から活用する実践デモです。

---

## 🎯 デモのハイライト

1. **Transactional Outbox パターンの完全な不要化（アトミック性の実証）**
   - 通常の RDBMS と外部 MQ（RabbitMQ, Apache Kafka 等）を組み合わせる場合、DB 更新と MQ 送信の不整合を防ぐために「Transactional Outbox パターン」や 2 相コミット（2PC/XA）が必要でした。
   - H2 Database Rust ではキュー自体がテーブルとして同一 MVStore トランザクション内に存在するため、**`@Transactional` 内で MyBatis の DB 更新と `JmsTemplate.convertAndSend()` を呼ぶだけで 100% アトミックにコミット／ロールバック**されます。
2. **Spring スタンダード JMS API への完全準拠**
   - 独自の API ではなく、標準の `jakarta.jms.*` および Spring JMS の `ConnectionFactory`, `JmsTemplate`, `@JmsListener` を介して透過的に操作できます。
3. **Kafka 風のオフセット再生（Seek / Replay）**
   - 従来の JMS キューと異なり、メッセージは読み出しても破棄されず、内部オフセット（`_offset`）で管理されます。
   - MyBatis XML Mapper から `WHERE _offset >= #{fromOffset}` を指定して SELECT することで、過去のメッセージをいつでも安全に巻き戻して再処理（Replay）できます。
4. **リテンション期間（RETENTION_HOURS）＆ 容量上限（MAX_BYTES）**
   - DDL 時に `WITH (RETENTION_HOURS = 24, MAX_BYTES = 10485760)` を指定可能。バックグラウンドの安全な自動 GC により、クリーンアップも自動化されます。

---

## 🏗️ アーキテクチャとクラス構成

```
demo/010_spring_boot_jms/
├── pom.xml                               # Java 21, Spring Boot 3.4.2 (4.1互換), MyBatis, Spring JMS
├── src/main/resources/
│   ├── application.yml                   # HikariCP, PostgreSQL JDBC 設定
│   └── mapper/
│       ├── OrderMapper.xml               # orders テーブル用 XML マッパー
│       └── QueueMapper.xml               # order_events_queue 用 XML マッパー (DDL, SELECT WHERE _offset >=)
└── src/main/java/com/example/jms/
    ├── SpringBootJmsDemoApplication.java # エントリポイント
    ├── config/
    │   └── JmsAndDbConfig.java           # @EnableJms, ConnectionFactory, JmsTemplate, DMLC 設定
    ├── listener/
    │   └── OrderEventListener.java       # @JmsListener による非同期メッセージ受信
    ├── mapper/
    │   ├── OrderMapper.java
    │   └── QueueMapper.java
    ├── model/
    │   ├── Order.java
    │   └── QueueMessage.java
    ├── provider/
    │   ├── H2JmsQueue.java               # jakarta.jms.Queue 実装
    │   ├── H2JmsTextMessage.java         # jakarta.jms.TextMessage 実装
    │   ├── H2JmsMessageProducer.java     # DataSourceUtils 連動 Producer
    │   ├── H2JmsMessageConsumer.java     # オフセット走査 Consumer
    │   ├── H2JmsSession.java             # jakarta.jms.Session 実装
    │   ├── H2JmsConnection.java          # jakarta.jms.Connection 実装
    │   └── H2JmsConnectionFactory.java   # ConnectionFactory 実装
    ├── runner/
    │   └── JmsDemoRunner.java            # 5 ステップの自動検証シナリオ
    └── service/
        └── OrderProcessingService.java   # @Transactional での DB更新 + JMS送信
```

---

## 🚀 実行手順

### 1. Rust 側サーバーの起動
別ターミナルで Rust サーバー（PostgreSQL ワイヤプロトコル対応）を起動します：
```bash
cargo run -p demo-spring-boot-server
```
サーバーがポート `5432` で待機します。

### 2. Spring Boot アプリケーションの実行
本ディレクトリで以下を実行します：

**Windows:**
```cmd
run_demo.bat
```
または
```cmd
mvn spring-boot:run
```

**Linux / macOS:**
```bash
chmod +x run_demo.sh
./run_demo.sh
```

---

## 📊 実証シナリオと実行ログ

`JmsDemoRunner` により、以下の 5 ステップが順次実行・検証されます：

1. **[Step 1] スキーマ初期化 (DDL)**
   - `OrderMapper.xml` による `orders` テーブルの作成
   - `QueueMapper.xml` による `CREATE QUEUE TABLE order_events_queue (...) WITH (RETENTION_HOURS = 24, MAX_BYTES = 10485760)` の作成
2. **[Step 2] アトミックコミット実証 (`@Transactional` + `JmsTemplate`)**
   - 注文レコード（ID: 101, Alice Johnson）を挿入し、同一トランザクション内で `jmsTemplate.convertAndSend(...)` を実行。
   - コミット後、`orders` テーブルと `order_events_queue` の両方に即座にデータが反映されます。
3. **[Step 3] アトミックロールバック実証 (障害時の完全取り消し)**
   - 注文レコード（ID: 999, Failed Customer）を挿入し、JMS メッセージをキューに追加後、業務例外を意図的にスロー。
   - `@Transactional` のロールバックにより、**DB の orders レコードだけでなく、JMS キューに投入されたメッセージも完全に取り消される**ことを確認。
   - これにより、メッセージ二重送信や配信先行による不整合（ファントムメッセージ）が完全に排除されます。
4. **[Step 4] `@JmsListener` による非同期受信**
   - Spring 標準の `@JmsListener(destination = "order_events_queue")` がバックグラウンドでメッセージを受信し、自動ディスパッチされることを確認。
5. **[Step 5] Kafka 風オフセット再生 (Seek / Replay)**
   - 追加の注文（ID: 102, 103）を投入。
   - `SELECT ... WHERE _offset >= 0` により、過去の全メッセージを順番通りに巻き戻して取得可能であることを実証。
   - `SELECT ... WHERE _offset >= 1` 等、指定オフセットからのシーク取得も実証。
