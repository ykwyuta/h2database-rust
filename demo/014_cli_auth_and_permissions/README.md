# Demo 014: ユーザー認証・アクセス元制限・テーブル権限管理 (DCL & Security)

このデモでは、専用 CLI (`h2-cli`) を用いて、h2database-rust に新設された以下のセキュリティ機構を実演します。

1. **ユーザー管理 (DCL)**:
   - `CREATE USER 'username' PASSWORD 'pass' HOST 'allowed_host';`
   - `ALTER USER 'username' PASSWORD 'new_pass' HOST 'new_host';`
   - `DROP USER 'username';`
   - `SHOW USERS;` (登録ユーザーおよび接続許可ホスト一覧表示)
2. **アクセス元ホスト／IP／CIDR 制限**:
   - `localhost` / `127.0.0.1`: ローカルマシンからの接続のみ許可
   - サブネット CIDR 表記（例: `192.168.1.0/24`）: 特定の社内ネットワークやセグメントからのみ許可
   - ワイルドカード `%` または `*`: 任意の接続元から許可
3. **テーブル単位の権限管理 (RBAC)**:
   - `GRANT <SELECT | INSERT | UPDATE | DELETE | ALL> ON TABLE <table_name> TO '<username>';`
   - `REVOKE <privileges> ON TABLE <table_name> FROM '<username>';`
   - `SHOW GRANTS FOR '<username>';`
   - クエリ実行時の厳格な権限チェック（権限がないテーブルへのアクセス時は即座に `H2Error::PermissionDenied` を送出）

---

## 実行方法

### Windows
```cmd
run.bat
```

### Linux / macOS
```bash
chmod +x run.sh
./run.sh
```

---

## CLI からのユーザー指定接続とセッション切り替え

### 1. ユーザーを指定して起動
```bash
cargo run -p h2-cli -- -u analyst -p new_analyst_2026 --db my_database.h2
```

### 2. 対話型シェル内でのユーザー切り替え
```sql
h2> .user
Current user: admin (unrestricted)

h2> .user analyst
Switched current session user to 'analyst'

h2> SELECT * FROM sales_data;
-- 許可されているため実行成功

h2> SELECT * FROM executive_payroll;
-- Permission denied: User 'analyst' does not have Select privilege on table 'executive_payroll'
```
