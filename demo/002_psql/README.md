# demo/002 psqlコマンド (PostgreSQL Wire Protocol & Client Connection)

本デモでは、`h2database-rust` の内蔵 **PostgreSQL v3 ワイヤプロトコルサーバー（PG-Wire）** を起動し、標準の **`psql`** コマンドラインツールや **DBeaver** などの外部ツールから接続して SQL を実行する方法を解説します。

---

## 💡 なぜ psql で接続できるのか？

`h2database-rust` は内部に PostgreSQL v3 互換のネットワークリスナー（`h2-server`）を組み込んでいます。
SSL/平文ハンドシェイク、スタートアップメッセージ、Simple Query プロトコル（`'Q'`）、CommandComplete、ReadyForQuery 等のパケットを完全互換で処理するため、**世界中のあらゆる PostgreSQL クライアントが無設定・無変更でそのまま接続可能**です。

---

## 🚀 クイックスタート手順

### ステップ 1: デモサーバーの起動

リポジトリルートから以下のコマンドを実行します。

```bash
cargo run -p demo-psql-server
# または run_server.bat
```

起動すると、ポート 5432 でリッスンが開始されます。

```text
============================================================
  H2 Database in Rust - PostgreSQL Wire Protocol Demo Server
============================================================
[INFO] Opened database at: psql_demo.h2
[INFO] Initialized demo data (3 server nodes).

  🚀 Server is listening on: 127.0.0.1:5432
  ----------------------------------------------------------
  Connect with psql:
    psql -h 127.0.0.1 -p 5432 -U postgres -d mydb
  ----------------------------------------------------------
```

---

### ステップ 2: 別ターミナルから `psql` で接続

```bash
psql -h 127.0.0.1 -p 5432 -U postgres -d mydb
# または connect_psql.bat
```

パスワードを聞かれた場合は、そのまま Enter（空パスワード）を押します。

接続に成功すると、おなじみの PostgreSQL プロンプトが表示されます。

```text
psql (16.2, server 15.0 (h2database-rust))
Type "help" for help.

mydb=> 
```

---

## 📝 実行できるクエリ例

本ディレクトリ内の [`demo.sql`](./demo.sql) を使って動作を確認できます。

### ① データの確認
```sql
mydb=> SELECT id, hostname, ip_address, status, cpu_usage FROM server_nodes;
 id |   hostname    |  ip_address  |  status  | cpu_usage 
----+---------------+--------------+----------+-----------
  1 | node-tokyo-01 | 192.168.1.10 | ONLINE   |      12.5
  2 | node-tokyo-02 | 192.168.1.11 | ONLINE   |      45.8
  3 | node-osaka-01 | 192.168.2.10 | DRAINING |       5.2
(3 rows)
```

### ② JSON 抽出オペレータ (`->>`)
```sql
mydb=> SELECT hostname, metadata ->> 'zone' AS zone, metadata ->> 'role' AS role
mydb-> FROM server_nodes
mydb-> WHERE metadata ->> 'role' = 'primary';
   hostname    |      zone      |  role   
---------------+----------------+---------
 node-tokyo-01 | ap-northeast-1a| primary
(1 row)
```

### ③ 集約関数と GROUP BY
```sql
mydb=> SELECT status, COUNT(*) AS count, AVG(cpu_usage) AS avg_cpu
mydb-> FROM server_nodes
mydb-> GROUP BY status;
  status  | count | avg_cpu 
----------+-------+---------
 ONLINE   |     2 |   29.15
 DRAINING |     1 |     5.2
(2 rows)
```

### ④ 明示的トランザクション
```sql
mydb=> BEGIN;
BEGIN
mydb=> UPDATE server_nodes SET status = 'MAINTENANCE' WHERE id = 3;
UPDATE 1
mydb=> COMMIT;
COMMIT
```

### ⑤ スクリプトの一括流し込み
```bash
psql -h 127.0.0.1 -p 5432 -U postgres -d mydb -f demo.sql
```

---

## 🖥️ GUI ツール (DBeaver / TablePlus / DataGrip) からの接続

1. **新規接続作成**: 「PostgreSQL」を選択
2. **ホスト (Host)**: `127.0.0.1` または `localhost`
3. **ポート (Port)**: `5432`
4. **データベース (Database)**: `mydb`（任意）
5. **ユーザー名 (Username)**: `postgres`（任意）
6. **パスワード (Password)**: 空欄
7. **接続テスト**: 「接続成功」と表示され、テーブルツリーから `server_nodes` や `information_schema` をGUI上で閲覧できます。
