# クエリ性能の計測と実行計画の確認

SQL 実行時間、ストレージ操作、待機時間を調べるための機能です。組み込み API と PGWire の Simple Query で利用できます。計測値はプロセス内に保持され、再起動で消えます。

## 実行計画と実測値

```sql
EXPLAIN SELECT balance FROM accounts WHERE id = 1;
EXPLAIN ANALYZE SELECT balance FROM accounts WHERE id = 1;
EXPLAIN ANALYZE UPDATE accounts SET balance = balance + 1 WHERE id = 1;
```

`EXPLAIN` は推定計画を表示し、対象文を実行しません。`EXPLAIN ANALYZE` は対象文を**実際に実行**し、計画に `Actual Rows`、`Execution Time`、待機時間、ストレージ操作回数を追加します。`UPDATE` などの変更は通常のトランザクション規則で適用されます。変更を残したくない場合は明示的なトランザクションで実行して `ROLLBACK` してください。

待機時間は次の内訳で表示します（単位 ms）。

| 項目 | 内容 |
| --- | --- |
| `row_lock` | キーのストライプロック取得および別トランザクションの終了待ち |
| `tree_lock` | MVMap のツリー読み取り・書き込みロック取得待ち |
| `commit_lock` | コミットのバージョンロック取得待ち |
| `wal_lock` | WAL マネージャーのロック取得待ち |
| `wal_write` | WAL ファイルの seek と書き込みにかかった時間 |
| `wal_sync` | `sync_data` にかかった時間 |

`point_gets` はトランザクションの単一キー取得回数、`scans` は走査呼び出し回数、`scan_entries` は走査で取得した**可視性判定前**のエントリ数です。これらと `IndexScan` / `TableScan` の計画を突き合わせると、想定外の全走査を見つけられます。時間は壁時計時間であり、CPU 使用時間ではありません。小さい値は小数第 3 位で丸めて表示します。

ロック待機は即時取得できなかった場合にだけ記録します。計測は現状すべての SQL で有効で、クエリの正規化と集計にも処理時間がかかります。性能への影響量はまだ測定していません。

現行の計画表示は SQL とカタログから組み立てる推定表示です。複雑な結合や最適化経路の各ノードについて、PostgreSQL と同等のノード別実測時間やバッファヒット数はまだ提供しません。`Execution Time` と待機時間の対象は対象文の実行区間で、暗黙の自動コミットは含みません。`EXPLAIN (ANALYZE, BUFFERS)` や JSON 形式などのオプションは未対応です。

## 累積クエリ統計

```sql
SHOW QUERY STATS;
RESET QUERY STATS;
```

`SHOW QUERY STATS` はクエリの実行回数、エラー数、結果行数、総・平均・最小・最大時間、待機時間の合計、ストレージ操作回数を総実行時間の大きい順に表示します。`RESET QUERY STATS` は累積値を消去します。認証が有効な PGWire 接続では、これらのコマンドはスーパーユーザーに限定されます。

数値リテラルと文字列リテラルを `?` に置換して同じクエリ形に集約します。文字列の内容は統計に保存しません。保持するクエリ形は最大 255 件で、それ以降の新しい形は `[other queries]` に集約します。`SHOW` と `RESET` 自体は統計に加えません。

組み込み API と PGWire の暗黙トランザクションでは、累積時間にトランザクションの開始とコミットを含みます。PGWire の明示的トランザクションでは各 SQL は文の実行時間のみを記録し、`COMMIT` の時間と WAL 待機は独立した `COMMIT` 行に集計します。組み込み API で `Transaction::commit` を直接呼ぶ場合、および PGWire の `ROLLBACK` は現在この統計に入りません。グループコミット導入後の `wal_sync_ms` は、同期を実行した代表スレッドで測った物理同期時間の合計です。`wal_durable_wait_ms` は各呼び出しが WAL 追記後に確定応答まで待った時間で、バッチ待機と確定値の公開も含みます。待機や書き込みの内訳は総時間と重なる区間であり、列を合計して総時間と比較する指標ではありません。

更新性能を比較する際は、[既存の pgbench SQL](../benchmarks/pgbench/point_update.sql)のような同一キー更新と、クライアントごとに異なるキーを更新する条件を分けて測ってください。行待機が大きければ競合、`wal_durable_wait_ms` が大きければ確定待ち、`scan_entries` が多ければ探索経路を優先して調べます。
