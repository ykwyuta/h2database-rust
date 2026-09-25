# Demo 009: Spring Boot 4.1 + MyBatis (XML Mapper) 統合デモ

H2 Database in Rust の全機能（高度なSQL、シーケンス、自動採番、区間型、数学/文字列関数、正規表現、カーソル走査、同期レプリケーションPrimary/Standby動的ルーティング）を、Javaのデファクトスタンダードである **Spring Boot + MyBatis（XMLマッパーにSQLを記述）** から網羅的に活用する実戦デモです。

---

## 🌟 実演する主要機能一覧

1. **MyBatis XML マッパー (`mapper/*.xml`) への SQL 記述**:
   - すべての SQL (DDL, SEQUENCE, DML, クエリ, 関数, 演算子) を XML マッパーに集約。
2. **Oracle / PostgreSQL スタイル `SEQUENCE` & 採番 (`NEXTVAL`)**:
   - `CREATE SEQUENCE user_id_seq INCREMENT BY 1 START WITH 1000`
   - `SELECT NEXTVAL('user_id_seq')`
3. **Primary / Standby 動的データソース切り替え (Read/Write Splitting)**:
   - Spring AOP と `AbstractRoutingDataSource` を組み合わせ、通常は Primary (Port 5432) へ書き込み、`@Transactional(readOnly = true)` が付与されたメソッドは自動的に Standby (Port 5433) へルーティング。
4. **Spring Transaction と Rollback**:
   - `@Transactional` 内で例外発生時に安全にロールバックされ、Standby 側にも不正なデータが同期されない ACID 保証。
5. **高度な数学・文字列関数**:
   - 自然対数 `LN(x)`, 指数 `EXP(x)`, 切り捨て `TRUNC(x, d)`
   - 単語先頭大文字化 `INITCAP(s)`, 正規表現置換 `REGEXP_REPLACE(s, pattern, repl)`
6. **POSIX 正規表現一致演算子 (`~`)**:
   - `WHERE metric_name ~ '^cpu_[0-9]+'`
7. **`SERIAL` 型 & `INTERVAL` 型演算**:
   - 自動連番主キー `SERIAL PRIMARY KEY`
   - 日時演算: `start_time + INTERVAL '30 DAYS'`
8. **MyBatis `ResultHandler` によるカーソル風ストリーミング走査**:
   - メモリを圧迫せずに大量レコードを順次フェッチ。

---

## 🚀 クイックスタート

### 1. レプリケーション・サーバーの起動 (Rust)

Primary (Port 5432) と Standby (Port 5433) を同時に起動し、ゼロラグ同期 (`remote_apply`) を開始します：

```bash
cargo run -p demo-spring-boot-server
```

出力例:
```text
[INFO] Primary instance started (Role: ReadWrite, Replication Hub: 127.0.0.1:xxxxx).
[INFO] Standby instance started (Role: ReadOnly, Synchronized with Primary).
  [Node 1] Primary (Read-Write) listening on: 127.0.0.1:5432
  [Node 2] Standby (Read-Only)  listening on: 127.0.0.1:5433
```

### 2. Spring Boot アプリケーションの実行 (Java 21 / Maven)

別ターミナルで実行します：

Windows:
```cmd
cd demo\009_spring_boot_mybatis
run_demo.bat
```

Linux / macOS:
```bash
cd demo/009_spring_boot_mybatis
./run_demo.sh
```

または直接 Maven で実行:
```bash
mvn spring-boot:run
```

---

## 📂 プロジェクト構成

```text
demo/009_spring_boot_mybatis/
├── pom.xml                                  # Spring Boot 3.4/4.x + MyBatis Starter
├── README.md                                # 本ガイド
├── run_demo.bat / run_demo.sh               # ワンクリック実行スクリプト
├── server/                                  # 2 ノード起動用 Rust サーバークレート
│   ├── Cargo.toml
│   └── src/main.rs
└── src/main/
    ├── java/com/example/mybatis/
    │   ├── SpringBootMybatisDemoApplication.java
    │   ├── config/
    │   │   ├── DataSourceConfig.java        # DynamicRoutingDataSource 定義
    │   │   ├── ReadOnlyRouteAspect.java     # @Transactional(readOnly=true) の自動検知
    │   │   └── RoutingDataSourceContext.java
    │   ├── mapper/
    │   │   ├── UserMapper.java              # ユーザ・シーケンス Mapper
    │   │   ├── AnalyticsMapper.java         # 数学・文字列・正規表現 Mapper
    │   │   └── SubscriptionMapper.java      # SERIAL・INTERVAL Mapper
    │   ├── model/
    │   │   ├── User.java
    │   │   ├── AnalyticsMetric.java
    │   │   └── Subscription.java
    │   ├── runner/
    │   │   └── MyBatisDemoRunner.java       # 全シナリオ実行 Runner
    │   └── service/
    │       └── UserService.java             # トランザクション制御サービス
    └── resources/
        ├── application.yml                  # Primary / Standby 接続先と HikariCP 設定
        └── mapper/
            ├── UserMapper.xml               # DDL, シーケンス, CRUD SQL
            ├── AnalyticsMapper.xml          # LN, EXP, TRUNC, ~, REGEXP_REPLACE SQL
            └── SubscriptionMapper.xml       # SERIAL, INTERVAL 加減算 SQL
```
