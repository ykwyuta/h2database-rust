# demo/006: 同期レプリケーション (PostgreSQL remote_apply 相当)

本デモは、1インスタンスを **Read-Write (Primary)**、もう1インスタンスを **Read-Only (Standby)** として起動し、PostgreSQL の `synchronous_commit = remote_apply` 相当の同期レプリケーション構成を実演します。

---

## 🌟 実演内容

1. **Primary & Standby の起動**:
   - Primary が指定ポート（または動的ポート）でリッスン
   - Standby が Primary に接続し、初期スナップショット（全マップ・カタログ）を自動同期
2. **即時同期保証 (`remote_apply`)**:
   - Primary で DDL（`CREATE TABLE`）や DML（`INSERT`, `UPDATE`, `DELETE`）を実行し、`COMMIT` が完了した瞬間には、すでに Standby 側でもデータが適用され参照可能
3. **Standby の読み取り専用ガード**:
   - Standby インスタンスに対するすべての書き込み操作（DDL, `INSERT`, `UPDATE`, `DELETE`）が自動的に `H2Error::ReadOnly` で安全に拒絶されることを確認
4. **明示的トランザクション（2フェーズ）の同期**:
   - `BEGIN` 〜 `COMMIT` による複数テーブル・行の更新が、Standby 側へ原子的に一括適用されることを確認

---

## 🏃 実行方法

```bash
cargo run -p demo-replication
```
