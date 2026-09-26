# 運用性レビュー: H2 Rust と主要 RDBMS の比較

確認日: 2026-09-26。対象: このリポジトリの現在の実装。**コードと既存テストの静的確認による評価**であり、[新規の運用性テスト30件](../../testcases/operation/README.md)の実行結果ではない。各製品の機能は構成・エディション・バージョンで異なるため、同じ SQL 構文の実装を要求せず、管理者が達成できる結果で比較する。

## 目標と判定尺度

運用上の最低線は、(1) 一貫したバックアップを検証・復元できる、(2) 障害や遅延の原因を稼働中と履歴から特定できる、(3) 悪化した計画を再現・制御できる、(4) 統計と不要領域が放置されない、(5) 保守の停止時間と失敗を監視できること。高度な機能である任意時点復旧、計画固定、自動計画回帰修正は、利用者の RPO/RTO と規模に応じて段階導入する。

記号: **基本あり** = 対応する API/SQL と既存単体テストあり、**部分的** = 運用目標の一部のみ、**未確認** = コード上に提供経路が見当たらない。基本ありでも今回の運用ケースの合格を意味しない。

| 領域 | PostgreSQL / SQL Server / Oracle の参考機能 | H2 Rust の現状 | 主な差分 | ケース |
| --- | --- | --- | --- | --- |
| バックアップ | ベースバックアップ＋WAL による PITR / full・log backup / RMAN と検証 | `BACKUP TO` の JSON ダンプ、`RESTORE FROM`、`SCRIPT TO` は基本あり | 一貫したオンライン取得、検証、復旧時点、復元の原子性が未確認 | BKP-01～06 |
| 監視 | 活動・待機・進捗のビュー / Query Store・DMV / V$・AWR | `SHOW QUERY STATS`、`EXPLAIN ANALYZE`、MCP `/health` は部分的 | 現在のブロッカー、保守進捗、再起動をまたぐ履歴、アラートが未確認 | MON-01～06 |
| 手動計画制御 | PostgreSQL の planner 設定 / SQL Server の Query Store plan forcing / Oracle の SQL Plan Baselines | `SET execution_mode` のみ基本あり | 特定 SQL の走査・結合方式誘導、計画固定・解除・失敗監視が未確認 | PLAN-01～06 |
| 自動最適化 | autovacuum/ANALYZE / 自動統計更新・自動計画修正 / 自動統計収集・SPM | 手動 `ANALYZE`、簡易コスト推定、実行方式 `auto` は部分的 | 変更量連動の統計更新、候補比較、計画回帰への自動対処が未確認 | AUTO-01～06 |
| VACUUM・断片化 | 通常 VACUUM と `VACUUM FULL` / インデックス再編成・再構築 / Segment Advisor・shrink | `VACUUM` と差分チェックポイント・64 差分後の履歴回収は部分的 | 全 DB 再構築中の書き込み停止、無駄領域の計測、対象別保守と進捗が未確認 | MAINT-01～06 |

## 1. バックアップ・復旧

PostgreSQL はベースバックアップと継続 WAL アーカイブを組み合わせて任意時点へ復旧できる。SQL Server も full と transaction log backup を使う復元系列を持ち、`RESTORE VERIFYONLY` でバックアップを検証できる。Oracle RMAN はバックアップ内容の検証と時刻/SCN/復元点への復旧を提供する。[PostgreSQL PITR](https://www.postgresql.org/docs/18/continuous-archiving.html)、[SQL Server のバックアップと復元](https://learn.microsoft.com/en-us/sql/relational-databases/backup-restore/back-up-and-restore-of-sql-server-databases?view=sql-server-ver17)、[RESTORE VERIFYONLY](https://learn.microsoft.com/en-us/sql/t-sql/statements/restore-statements-verifyonly-transact-sql?view=sql-server-ver17)、[Oracle RMAN の検証](https://docs.oracle.com/en/database/oracle/oracle-database/19/bradv/validating-database-files-backups.html)、[Oracle PITR](https://docs.oracle.com/en/database/oracle/oracle-database/19/bradv/rman-performing-flashback-dbpitr.html)。

現行の [`dump_backup` / `restore_backup`](../../../crates/h2-mvstore/src/store.rs) は、各マップの生エントリを順に JSON 化し、復元時に既存マップへ `clear`/`put` して最後にコミットする。既存の [`test_backup_and_restore`](../../../crates/h2/tests/backup_copy_cursor_tests.rs) は静止時の少数行を確認する。一方、バックアップには共通スナップショット番号、形式バージョン、チェックサム、WAL の復旧境界、完了マニフェストがない。マップを順に走査するため、並行コミット下の表間整合性は保証を確認できない。復元はバックアップに存在しない既存マップを削除せず、途中エラー時の全体巻き戻しも見当たらない。`SCRIPT TO` は別の論理形式であり、物理バックアップや PITR の代替として扱わない。

**優先度 P0:** BKP-02/04/05 で整合性・破損時の非破壊性・置換意味を先に確かめる。スナップショット境界と検証可能なマニフェスト、別 DB への検証復元、原子的な切り替えを設計する。PITR は WAL の保存・世代管理・復元インターフェースを伴う別段階として BKP-03 で検証する。

## 2. 監視・診断

PostgreSQL は `pg_stat_activity` で現在の活動を、進捗ビューで VACUUM/ANALYZE などを観測し、`pg_stat_statements` でクエリ統計を集約できる。SQL Server の Query Store は計画・実行統計・待機統計の履歴を保持し、`sys.dm_exec_requests` は現在の待機とブロッカーを示す。Oracle は `V$SESSION` などの動的ビューと AWR/ADDM を持つ。[PostgreSQL 統計](https://www.postgresql.org/docs/18/monitoring-stats.html)、[進捗](https://www.postgresql.org/docs/18/progress-reporting.html)、[pg_stat_statements](https://www.postgresql.org/docs/18/pgstatstatements.html)、[SQL Server Query Store](https://learn.microsoft.com/en-us/sql/relational-databases/performance/monitoring-performance-by-using-the-query-store?view=sql-server-ver17)、[DMV](https://learn.microsoft.com/en-us/sql/relational-databases/system-dynamic-management-views/sys-dm-exec-requests-transact-sql?view=sql-server-ver17)、[Oracle V$SESSION](https://docs.oracle.com/en/database/oracle/oracle-database/19/refrn/V-SESSION.html)、[AWR/ADDM](https://docs.oracle.com/en/database/oracle/oracle-database/19/tgdba/automatic-performance-diagnostics.html)。

現行の [`QueryStats`](../../../crates/h2-sql/src/query_stats.rs) はリテラルを伏せた形で最大 255 クエリをメモリ集計し、再起動で消える。[`EXPLAIN ANALYZE`](../../../crates/h2-sql/src/executor.rs) の実測は文全体の行数・時間・待機・ストレージ操作であり、ノード別値やバッファ情報はない。MCP の [`/health`](../../../crates/h2-mcp/src/server.rs) は基本状態とバージョンだけを返す。活動中のセッション・ブロッカー・バックアップ/保守進捗を運用者に示すビューや、期間比較できる永続履歴は見当たらない。PGWire は `SHOW QUERY STATS` 以外の `SHOW` を一部固定値で返すため、組み込み API の `SHOW WORK_MEM` 等と結果が一致するかも確認が必要（[`server.rs`](../../../crates/h2-server/src/server.rs)）。

**優先度 P0:** MON-02/03 でライブ待機と計画の信頼性を検証する。セッション/要求 ID、待機・ブロッカー、操作進捗、ストレージ/WAL 指標を追加し、履歴は保持期間と公開権限を定義する。MON-04～06 で再起動・監視負荷・権限を検証する。

## 3. 実行計画の手動制御

PostgreSQL は `enable_seqscan` などで planner の候補を**誘導**できるが、完全な計画固定とは異なる。SQL Server Query Store は計画の履歴と plan forcing、解除・失敗情報を持つ。Oracle SQL Plan Management は受け入れた計画の baseline と進化を管理する。[PostgreSQL planner 設定](https://www.postgresql.org/docs/18/runtime-config-query.html)、[SQL Server Query Store](https://learn.microsoft.com/en-us/sql/relational-databases/performance/monitoring-performance-by-using-the-query-store?view=sql-server-ver17)、[Oracle SQL Plan Management](https://docs.oracle.com/en/database/oracle/oracle-database/19/tgsql/overview-of-sql-plan-management.html)。

現行の [`SET execution_mode`](../../../crates/h2-sql/src/executor.rs) は `auto` / `row` / `vectorized` を切り替える。設定値は `SQLEngine` の共有状態にあり、セッション専用とは限らない。索引/結合順序のヒントや保存された物理計画の強制・解除 API は見当たらない。`plan_cache` は SQL を解析した AST のキャッシュであり、実行計画履歴ではない。`explain_statement` は索引条件を別途判定して文字列を組み立てるため、実行経路との一致は PLAN-01 で実測確認すべきである。

**優先度 P1:** まず EXPLAIN と実行器が同じ計画オブジェクトを共有し、PLAN-01 を満たす。その後にセッション/クエリ単位の誘導、計画 ID と固定・解除・無効時のフォールバックを追加する。固定は自動最適化を妨げるため、適用理由・期限・性能の再評価を伴わせる。

## 4. 自動最適化と統計

PostgreSQL の autovacuum は変更量を見て `ANALYZE` を実行する。SQL Server は統計の自動更新を持ち、設定すれば Query Store を使った「最後の良好計画」への自動修正ができる。Oracle は保守時間帯の自動統計収集と、設定に応じた SQL Plan Management の計画進化を提供する。これらはすべて無条件に同じ構成で有効という意味ではない。[PostgreSQL routine vacuuming](https://www.postgresql.org/docs/18/routine-vacuuming.html)、[SQL Server 統計](https://learn.microsoft.com/en-us/sql/relational-databases/statistics/statistics?view=sql-server-ver17)、[SQL Server 自動調整](https://learn.microsoft.com/en-us/sql/relational-databases/automatic-tuning/automatic-tuning?view=sql-server-ver17)、[Oracle 統計収集](https://docs.oracle.com/en/database/oracle/oracle-database/19/tgsql/gathering-optimizer-statistics.html)、[Oracle 計画管理](https://docs.oracle.com/en/database/oracle/oracle-database/19/tgsql/managing-sql-plan-baselines.html)。

現行の [`ANALYZE`](../../../crates/h2-sql/src/executor.rs) は手動実行で、[`stats.rs`](../../../crates/h2-sql/src/stats.rs) が表を全走査し、列ごとの distinct 値を線形探索で数えて MCV 上位 10 件を保存する。大きな高カーディナリティ列では時間・メモリが増える可能性がある。コスト・選択率の式はあるが、`EXPLAIN` 側で算出する見積もりと実際のアクセス経路選択は別経路で、複数の物理候補を系統的に比較する最適化器とはまだ言えない。結合駆動表の簡易選択はある。変更量に応じた自動統計更新、統計の鮮度指標、計画回帰の検出・自動解除は見当たらない。

**優先度 P0:** AUTO-01～03 で統計の永続性、更新後の鮮度、選択率と実行経路の対応を確認する。統計収集はサンプリング/メモリ上限と変更量閾値を設ける。計画履歴が取れるようになってから回帰検出・自動修正を評価する。

## 5. VACUUM・断片化・容量

PostgreSQL の通常 `VACUUM` は古い行版を回収して再利用可能にし、ファイル縮小を要する `VACUUM FULL` は別の重い操作である。SQL Server は断片化率に加えてページ密度を観測し、必要に応じてインデックスを reorganize/rebuild する。Oracle は Segment Advisor とオンライン shrink で回収候補を判断できる。[PostgreSQL VACUUM](https://www.postgresql.org/docs/18/routine-vacuuming.html)、[SQL Server インデックス保守](https://learn.microsoft.com/en-us/sql/relational-databases/indexes/reorganize-and-rebuild-indexes?view=sql-server-ver17)、[Oracle Segment Advisor と shrink](https://docs.oracle.com/en/database/oracle/oracle-database/19/admin/managing-space-for-schema-objects.html)。

現行の [`MVStore::compact`](../../../crates/h2-mvstore/src/store.rs) は全マップを走査し、新しい全量ファイルに置換して WAL を切り詰める。`checkpoint_gate` とマップ凍結で書き込みを止めるので、「オンライン VACUUM」という既存テスト名だけから無停止とは判断できない。差分チェックポイントと 64 差分後の自動履歴回収はあるが、実行スケジュールは呼び出し側に依存する。[デモサーバー](../../../demo/002_psql/src/main.rs) は WAL サイズ/時間でチェックポイントを呼ぶが、自動 `VACUUM` は呼ばない。表/索引単位の無駄領域、回収見込み、保守進捗、長寿命スナップショットによる回収抑制を示す指標は見当たらない。

**優先度 P0:** MAINT-02/03/06 でスナップショット安全性、停止時間、失敗時復旧を確認する。通常の不要版回収とファイル縮小の重い再構築を区別し、MAINT-04 の指標で保守を判断する。単純な断片化率だけで再構築しないことは SQL Server の保守資料とも整合する。

## 実装と検証の順序

1. **復元の正しさ:** 同一スナップショットのバックアップ、マニフェストと検証、隔離先での復元、原子的な切り替え。BKP-02/04/05 と MAINT-06 をゲートにする。
2. **観測の信頼性:** 実行器と EXPLAIN の計画共有、ライブ待機/ブロッカー、保守・WAL の状態、権限付き履歴。MON-02/03/05 と PLAN-01 をゲートにする。
3. **自動保守の基盤:** 統計の鮮度・変更量・資源上限、不要版と無駄領域の指標、保守時間帯と進捗。AUTO-01/02/06 と MAINT-02～05 をゲートにする。
4. **高度な復旧と計画制御:** WAL 保存による PITR、計画履歴と固定・解除、回帰検出と自動復帰。BKP-03、PLAN-04/05、AUTO-05 をゲートにする。

現時点では各ケースは `NOT RUN`。静的確認で見つからない機能は「未確認」としており、未実装の断定と実行時不具合の確定は、ケース実行後に更新する。
