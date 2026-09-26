# 運用性テストケース

対象は H2 Rust の組み込み API、PGWire、ファイル DB。これは**テスト仕様であり、実行結果ではない**。現行実装で未提供の機能も、運用上必要な合格条件として残す。コード確認に基づく差分は [運用性レビュー](../../review/operation/README.md) に記す。

| 分野 | ケース |
| --- | --- |
| バックアップ・復旧 | [BKP-01～06](backup_recovery.md) |
| 監視・診断 | [MON-01～06](monitoring.md) |
| 実行計画の手動制御 | [PLAN-01～06](plan_control.md) |
| 自動最適化 | [AUTO-01～06](automatic_optimization.md) |
| VACUUM・断片化・容量 | [MAINT-01～06](maintenance.md) |

## 共通条件と記録

1. テストごとに独立した一時ディレクトリのファイル DB を使う。メモリ DB だけでバックアップ・再起動を判定しない。実行環境、コミット ID、`H2_SYNC_COMMIT`、DB/WAL のパスを記録する。
2. `ops_data(id INT PRIMARY KEY, grp INT, payload VARCHAR)` に連番 1～10,000、`ops_small(id INT PRIMARY KEY, note VARCHAR)` に 10 行を入れる。`ops_data.grp` は偏りを作る（9,000 行が 0、残りが 1～10）。必要なケースでは `CREATE INDEX idx_ops_grp ON ops_data(grp)` を実行する。各テストでデータを作り直す。
3. 観測値は開始・終了時刻、行数・チェックサム相当の値、DB/WAL/バックアップのバイト数、応答時間 p50/p95/p99、エラー数を残す。並行操作は別セッションで行い、書き込みの成否と復元後の内容を照合する。
4. `PASS` は期待結果を実測で満たした場合のみ。未実装でコマンドが拒否された場合、代替手段が指定した運用目標を満たさなければ `FAIL` とする。実行できない環境は理由付き `BLOCKED`、未実行は `NOT RUN`。失敗したコマンドが成功したように返る場合も `FAIL`。
5. 破壊的操作（復元、ファイル破損、強制終了）は一時 DB のコピーで行う。`EXPLAIN ANALYZE` の変更文は実行されるため、ロールバック可能なトランザクション内でだけ試す。

このテスト群は PostgreSQL、SQL Server、Oracle の SQL 構文互換性を要求するものではない。バックアップの整合性、診断可能性、計画変更の制御、保守の可視性といった**運用上の結果**を検証する。
