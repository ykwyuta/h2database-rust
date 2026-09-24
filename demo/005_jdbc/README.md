# demo/005 JavaのJDBCドライバからの利用 (Java JDBC Client via PostgreSQL Driver)

本デモでは、Java アプリケーションから**標準の PostgreSQL JDBC ドライバ（`org.postgresql.Driver`）**を使用して、`h2database-rust` に接続・操作する方法を解説します。

---

## 💡 Java 開発者・H2 ユーザーにとってのメリット

元祖 Java 版 H2 Database は Java 開発者の間で広く愛用されてきました。
本 Rust 版 H2 Database は、内蔵する PostgreSQL ワイヤプロトコル（PG-Wire）により、**Java 公式の PostgreSQL JDBC ドライバを使ってそのまま接続できます**。
- **Spring Boot**, **Quarkus**, **Micronaut**, **Hibernate / JPA**, **MyBatis**, **JOOQ** などの Java 生態系ツールから通常のリモート DB またはローカル DB として利用可能。
- Java VM のガベージコレクション（GC）停止に悩まされない、Rust のゼロコスト抽象化・低メモリフットプリントなストレージの恩恵を享受。

---

## 🚀 実行手順

### ステップ 1: H2 サーバーの起動

まず、バックグラウンドまたは別ターミナルで PostgreSQL 互換サーバーを起動しておきます。

```bash
# リポジトリルートから
cargo run -p demo-psql-server
```

ポート `5432` でリッスンが開始されます。

---

### ステップ 2: Java デモのビルド＆実行

Maven がインストールされている環境で、本ディレクトリ内で以下を実行します。

```bash
cd demo/005_jdbc
mvn compile exec:java
# または run_java.bat / run_java.sh
```

---

## 📝 接続設定とコード例

ソースコード: [`src/main/java/com/example/H2JdbcDemo.java`](./src/main/java/com/example/H2JdbcDemo.java)

### 1. Maven の依存関係 (`pom.xml`)
```xml
<dependency>
    <groupId>org.postgresql</groupId>
    <artifactId>postgresql</artifactId>
    <version>42.7.4</version>
</dependency>
```

### 2. 接続の確立
```java
String url = "jdbc:postgresql://localhost:5432/mydb";
String user = "postgres";
String password = ""; // パスワード不要

Connection conn = DriverManager.getConnection(url, user, password);
```

### 3. PreparedStatement と型バインド
```java
String sql = "INSERT INTO inventory VALUES (?, ?, ?, ?)";
try (PreparedStatement pstmt = conn.prepareStatement(sql)) {
    pstmt.setInt(1, 101);
    pstmt.setString(2, "Gaming Laptop");
    pstmt.setBigDecimal(3, new BigDecimal("1499.99"));
    pstmt.setInt(4, 15);
    pstmt.executeUpdate();
}
```

### 4. トランザクション制御 (Commit & Rollback)
```java
conn.setAutoCommit(false);

stmt.executeUpdate("UPDATE inventory SET stock = stock - 5 WHERE item_id = 101");
conn.rollback(); // 変更を安全に破棄

stmt.executeUpdate("UPDATE inventory SET stock = stock + 10 WHERE item_id = 102");
conn.commit();   // 変更を確定

conn.setAutoCommit(true);
```

---

## ☕ Spring Boot での設定例 (`application.yml`)

Spring Boot アプリケーションで利用する場合の `application.yml` の設定例です：

```yaml
spring:
  datasource:
    driver-class-name: org.postgresql.Driver
    url: jdbc:postgresql://localhost:5432/mydb
    username: postgres
    password: ""
  jpa:
    database-platform: org.hibernate.dialect.PostgreSQLDialect
    hibernate:
      ddl-auto: update
```
