# MCP HTTP・ツールテストケース

共通の準備と判定方法は [README](README.md) を参照。HTTP 要求は `/mcp` への `tools/call` を使用する。基準形は `{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"query_read","arguments":{"sql":"SELECT 1"}}}`。拒否判定では HTTP ステータスだけでなく JSON-RPC の結果と DB の事後状態を確認する。

## MCP-01 未認証要求と Origin（P0）

- 手順: 認証ヘッダーなしで `tools/list`、`query_read`、`query_write`、`resources/read` を呼ぶ。`Origin: https://attacker.example` を付けて CORS 応答も確認する。書き込み許可構成でも繰り返す。
- 合格基準: 公開可能な構成では未認証者にデータ・スキーマ・統計・書き込み機能を渡さない。任意 Origin から秘密を読める CORS 応答を返さない。

## MCP-02 query_read の複数文（P0）

- 手順: `query_read` に `SELECT * FROM public_data; UPDATE public_data SET value='changed' WHERE id=1`、および `SELECT 1; CREATE TABLE injected(id INT)` を渡す。
- 合格基準: 入力全体が読み取り専用として検査される。書き込みを含む要求は拒否され、値とスキーマが変わらない。

## MCP-03 EXPLAIN ANALYZE による書き込み（P0）

- 手順: `query_read` に `EXPLAIN ANALYZE UPDATE public_data SET value='changed' WHERE id=1` を渡す。同じ入力を `explain_query` の `sql` にも渡す。
- 合格基準: 読み取り用ツールから変更文は実行されず、値は変わらない。通常の `EXPLAIN SELECT` は引き続き利用できる。

## MCP-04 CTE・コメント・大小文字の回避（P0）

- 手順: `query_read` へ `WITH x AS (UPDATE public_data SET value='changed' RETURNING id) SELECT * FROM x`、`SELECT 1; /*comment*/ DELETE FROM public_data`、改行・タブ・大小文字を変えた同等入力を送る。
- 合格基準: 字句の見た目に依存せず、実行される全文と CTE の変更操作を拒否する。データは変わらない。

## MCP-05 query_write の無効化と dry_run（P0）

- 手順: 既定の `allow_write=false` で `query_write` の通常実行と `dry_run=true` を使い、`UPDATE` と `CREATE TABLE` をそれぞれ試す。次に `allow_write=true` で dry run を繰り返す。
- 合格基準: 書き込みを許可していない構成では dry run を含め永続変更がない。dry run は成功・失敗の両方で DML、DDL、カタログ変更を残さない。

## MCP-06 応答行数・バイト数の制限（P1）

- 手順: `max_rows=0`、`5000`、非常に大きい整数を与え、多数の行と長い文字列を返す `SELECT` を実行する。`max_response_bytes` を小さくした構成でも実行する。
- 合格基準: 行数とレスポンスバイト数は設定上限以下。制限適用前の全件実体化でメモリが枯渇しない。切り詰めたことを結果で識別できる。

## MCP-07 タイムアウトの下限・上限（P1）

- 手順: `timeout_ms=0`、極大値、通常値を与え、遅い結合・大量行を読むクエリを実行する。その後で `SELECT 1` を実行する。
- 合格基準: 呼び出し側が無制限の実行を強制できない。時間超過後に処理とロックが解放され、次の要求を処理できる。

## MCP-08 ツール引数を介した SQL・表示の混入（P1）

- 手順: `describe_table` の `table_name` に `public_data\"; DROP TABLE secret_data; --` を渡し、テーブル名・セル値に改行、`|`、Markdown/HTML 断片を含むデータを返させる。
- 合格基準: 識別子入力で追加 SQL が実行されない。表示形式で値が構造や指示文へ化けず、テーブルとデータに変化がない。
