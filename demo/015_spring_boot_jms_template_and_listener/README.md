# Demo 015: Spring Boot 4.1 / Spring JMS (JmsTemplate & @JmsListener) Patterns Demo

このデモは、Java の標準エンタープライズフレームワークである **Spring JMS** の主要な活用パターン（**`JmsTemplate`** と **`@JmsListener`**）を、H2 Database Rust の**トランザクショナル・キューテーブル（`CREATE QUEUE TABLE`）**上で実演する専用のデモプロジェクトです。

---

## 🎯 実演されている 5 つの JMS パターン

1. **パターン 1: `JmsTemplate.convertAndSend` ＋ `@JmsListener` による非同期購読と `@Header` のプロパティ注入**
   - POJO（`NotificationMessage`）を自動的に JSON テキストメッセージにシリアライズして送信。
   - `MessagePostProcessor` でカスタムヘッダー（`priorityLevel`, `sourceApp`）を付与。
   - コンシューマ側の `@JmsListener` では、`@Payload` と `@Header` アノテーションにより自動的に引数へバインドされます。

2. **パターン 2: `@SendTo` アノテーションによる Request-Reply (RPC) パターン**
   - リクエストキュー（`payment_request_queue`）へ `PaymentRequest` を送信。
   - `@JmsListener` がリクエストを受け取って決済承認処理を行い、戻り値として `PaymentResponse` を返却。
   - `@SendTo("payment_reply_queue")` により、Spring JMS がレスポンスキューへ自動的に返信メッセージをルーティングします。

3. **パターン 3: `JmsTemplate.receiveAndConvert` による同期ブロッキング受信**
   - `@JmsListener` によるプッシュ型受信だけでなく、ポーリング型のコンシューマとして `receiveAndConvert(timeout)` を用いた同期的なメッセージ取得を実演します。

4. **パターン 4: `@Transactional` による JMS 送信ロールバックの実証**
   - Spring の `@Transactional` 境界内でメッセージをキューへ送信し、直後に例外が発生した場合、H2 のトランザクショナルキューの原子性により、送信したメッセージが完全にロールバックされることを実証します。

5. **パターン 5: Kafka 風のオフセット再生（Seek / Replay）**
   - 一般的な JMS ブローカーでは読み出されたメッセージは消去されますが、H2 Database Rust では内部オフセット（`_offset`）で管理されるため、`SELECT * FROM queue WHERE _offset >= n` により、過去のメッセージをいつでも再取得・再生可能です。

---

## 🏗️ プロジェクト構成

```
demo/015_spring_boot_jms_template_and_listener/
├── pom.xml                                   # Spring Boot 3.4.2 (4.1互換), Spring JMS, Jackson
├── src/main/resources/
│   └── application.yml                       # HikariCP, PostgreSQL Driver 設定
└── src/main/java/com/example/jms/
    ├── SpringBootJmsPatternsApplication.java # エントリポイント
    ├── config/
    │   └── JmsConfig.java                    # @EnableJms, JmsTemplate, Jackson MessageConverter
    ├── listener/
    │   ├── NotificationListener.java         # @JmsListener + @Header パターン
    │   └── PaymentProcessorListener.java     # @JmsListener + @SendTo (RPC) パターン
    ├── model/
    │   ├── NotificationMessage.java          # 通知 POJO
    │   ├── PaymentRequest.java               # 決済リクエスト POJO
    │   └── PaymentResponse.java              # 決済レスポンス POJO
    ├── provider/                             # H2 Database Rust 用 Jakarta JMS 実装
    │   ├── H2JmsConnectionFactory.java
    │   ├── H2JmsConnection.java
    │   ├── H2JmsSession.java
    │   ├── H2JmsQueue.java
    │   ├── H2JmsTextMessage.java
    │   ├── H2JmsMessageProducer.java
    │   └── H2JmsMessageConsumer.java
    ├── runner/
    │   └── JmsPatternsDemoRunner.java        # 5 パターンの自動実行・検証ランナー
    └── service/
        └── JmsClientService.java             # JmsTemplate クライアントサービス
```

---

## 🚀 実行手順

### 1. Rust サーバーの起動
別ターミナルで Rust サーバーを起動します：
```bash
cargo run -p demo-spring-boot-server
```

### 2. Spring Boot JMS パターンデモの実行
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
