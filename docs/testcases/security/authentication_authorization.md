# 認証・権限テストケース

共通の準備と判定方法は [README](README.md) を参照。`reader` の操作は新しいセッションまたは PGWire 接続で実施し、管理者セッションを流用しない。

## AUTH-01 既定管理者へのパスワードなし接続（P0）

- 手順: 認証設定を追加していない新規 DB の PGWire に、`admin`、`postgres`、`sa`、`root`、`app` の各ユーザー名をパスワードなしで指定して接続し、`SELECT` と `CREATE TABLE` を試す。
- 合格基準: ネットワーク公開可能な構成では、明示的に設定していない管理者として認証・管理操作できない。拒否後にスキーマが変わらない。

## AUTH-02 誤った資格情報とユーザー列挙（P0）

- 手順: パスワードを設定した `reader` に対し、正しい値、誤った値、空文字、未指定、存在しないユーザー名を順に送る。
- 合格基準: 正しい資格情報だけ成功する。失敗時にセッションが作られず、応答からパスワードや過度なアカウント存在情報が漏れない。

## AUTH-03 ホスト制限の境界（P1）

- 手順: `127.0.0.1`、`192.168.10.0/24`、`localhost` を許可したユーザーを別々に作り、`127.0.0.1`、`::1`、`192.168.10.255`、`192.168.11.1` からの接続を検証する。ユニット層では `authenticate(username, password, client_ip)` に各アドレスを与える。
- 合格基準: 許可範囲だけ成功する。IPv4 と IPv6、CIDR の境界を混同せず、拒否時に権限を得ない。

## AUTH-04 他表・他操作への権限の広がり（P0）

- 手順: `reader` に `public_data` の `SELECT` だけ付与し、同表の `INSERT` / `UPDATE` / `DELETE`、`secret_data` の `SELECT`、両表の `JOIN` とサブクエリを試す。
- 合格基準: 許可された読み取りだけ成功する。結合・サブクエリから `secret_data.value` が返らず、拒否後にデータが変わらない。

## AUTH-05 権限剥奪と既存セッション（P0）

- 手順: `reader` の接続後に管理者が `REVOKE SELECT ON TABLE public_data FROM 'reader'` を実行し、既存接続と新規接続の両方で再度 `SELECT` する。
- 合格基準: 剥奪後の双方の実行が拒否される。以前取得した結果や計画キャッシュを使っても読めない。

## AUTH-06 独自管理コマンドの権限確認（P0）

- 手順: `reader` で `CREATE USER 'elevated' PASSWORD 'dummy'`、`ALTER USER 'reader' PASSWORD 'changed'`、`GRANT SELECT ON TABLE secret_data TO 'reader'`、`REVOKE`、`SHOW USERS`、`SHOW GRANTS FOR 'admin'` を実行する。
- 合格基準: 管理者専用の変更・一覧取得は拒否される。ユーザー情報と権限は変化しない。

## AUTH-07 ファイル操作・管理操作の権限確認（P0）

- 手順: 一時ディレクトリ内にカナリアファイルを用意し、`reader` で `BACKUP TO`、`RESTORE FROM`、`SCRIPT TO`、`RUNSCRIPT FROM`、`VACUUM`、`SET MAX_MATERIALIZED_ROWS` を試す。パスには一時ディレクトリ内とその外側のテスト専用カナリアを指定する。
- 合格基準: 一般ユーザーの管理操作、DB 外の読み書き、設定変更が拒否される。カナリアと DB 内容に変化がない。

## AUTH-08 EXPLAIN と統計表示の権限（P1）

- 手順: `reader` で `EXPLAIN SELECT * FROM secret_data`、`EXPLAIN ANALYZE SELECT * FROM secret_data`、`SHOW QUERY STATS`、`RESET QUERY STATS` を試す。管理者では同じ操作の許可も確認する。
- 合格基準: 一般ユーザーは保護表の計画・実測結果と全体のクエリ統計を取得・消去できない。拒否時に統計とデータは変わらない。
