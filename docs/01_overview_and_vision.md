# 01. プロジェクトビジョンと技術比較

## 1. ビジョン: 「SQLiteの使い勝手 × PostgreSQLの機能・並行性 × Rustの堅牢性」

### 背景と課題
現在、アプリケーション組み込み用データベースとしては **SQLite** がデファクトスタンダードとして君臨しています。
SQLiteは「設定不要」「単一ファイル」「軽量・高速」という非常に優れた体験を提供する一方で、以下の限界を抱えています：

1. **書き込みの並行性限界**:
   - WAL (Write-Ahead Logging) モードであっても、**同時書き込みトランザクションは常に1つ**に制限されます。複数のWebリクエストやバックグラウンドタスクが同時に書き込もうとすると、`SQLITE_BUSY` エラーや待機が発生します。
2. **型システムの緩さ (Type Affinity)**:
   - 動的型付けであるため、INTEGER列に文字列を書き込んでもエラーにならず、データ不整合の原因になりやすいです。また、DECIMAL/NUMERIC型の正確な固定小数点計算やネイティブUUID、タイムゾーン付き日時の扱いが弱いです。
3. **外部ツールからのアクセシビリティ**:
   - 組み込み専用であるため、稼働中のアプリケーションのデータベースを外部のGUI（DBeaverなど）から参照・操作することが難しく、運用・デバッグのハードルがあります。
4. **最新のデータワークロードへの対応**:
   - AI/LLM連携のためのベクトル検索（Vector Similarity Search）、リッチなJSON操作、高度な分析クエリ（Window関数、CTE）などを標準で快適に使うには拡張モジュールのビルドや設定が必要です。

### Java版 H2 Database が持っていた先進性
H2 DatabaseはJavaエコシステムにおいて、長年「インメモリテスト用」および「組み込み／軽量RDBMS」として愛用されてきました。
H2には以下のような非常に優れた特徴がありました：
- **MVStore**: ログ構造化＋追記型 B-Tree（Copy-on-Write）を採用し、軽量かつ優れたMVCC（マルチバージョン並行性制御）を実現。
- **リッチなSQL標準対応**: 厳格なデータ型、CTE、Window関数、シーケンス、トリガーなどを網羅。
- **PostgreSQL互換プロトコルサーバー内蔵**: 組み込み動作しながら、TCP経由でPostgreSQLドライバやツールから接続可能。
- **互換性モード**: MySQL, PostgreSQL, Oracle等の構文・動作を模倣可能。

しかしJava版は、JVMのメモリフットプリント、GCの一時停止、Javaランタイムへの依存といった制約があり、Rust/C/Goなどの他言語から「SQLiteのようにC-ABI/バイナリ単体で組み込む」用途には向いていませんでした。

### h2-rust の目指す姿
Java版H2の設計思想（特に**MVStore**と**PostgreSQL互換性**、**高いSQL互換性**）をRustで再構築し、SQLiteの手軽さで使え、かつSQLiteの限界を打ち破る次世代データベースを実現します。

---

## 2. 徹底機能比較表

| 項目 | SQLite | Java版 H2 Database | **本プロジェクト (h2-rust)** |
| :--- | :--- | :--- | :--- |
| **開発言語** | C | Java | **Rust (メモリ安全, ゼロコスト抽象化)** |
| **ランタイム依存** | なし（Cライブラリ） | JVM (JRE/JDK) | **なし（単一バイナリ / Rustクレート）** |
| **組み込み利用** | ◎ 最も得意 | ○ (Javaアプリ内のみ) | **◎ `cargo add h2-rust` で即座に利用可能** |
| **サーバーモード** | × なし (サードパーティ製を除く) | ◎ TCP / PG / Webサーバー | **◎ 内蔵TokioベースPGワイヤプロトコル** |
| **並行制御 (CC)** | 単一ライターロック (WAL時も1書込) | MVCC (MVStore / 行ロック) | **MVCC (追記型CoW B-Tree / 並行書込OCC)** |
| **トランザクション分離** | SERIALIZABLE, READ UNCOMMITTED | READ COMMITTED, SERIALIZABLE | **READ COMMITTED, SNAPSHOT ISOLATION, SERIALIZABLE** |
| **型システム** | 動的 (Type Affinity, 緩い型) | 厳格 (SQL標準型) | **厳格型 + Rust型安全 + PG互換型** |
| **Decimal (高精度計算)** | × 浮動小数点または文字列 | ◎ BigDecimalネイティブ | **◎ `rust_decimal` ネイティブ高精度計算** |
| **日付・時刻** | × 文字列/数値の規約依存 | ◎ TIMESTAMP WITH TIME ZONE | **◎ `chrono` ネイティブ、TZ・マイクロ秒対応** |
| **JSON / JSONB** | △ 拡張・関数ベース | ○ JSON型 | **◎ ネイティブJSONBバイナリ形式 + パス抽出** |
| **ベクトル検索 (AI対応)** | × sqlite-vec等外部ビルド必要 | × 標準未対応 | **◎ コア組込み (HNSW / Cosine / L2距離)** |
| **非同期I/O対応** | × (スレッドプールでラップ必須) | × (同期JDBCのみ) | **◎ Tokio非同期ネイティブ ＋ 同期ラッパー両対応** |
| **外部GUIツール接続** | △ ファイル排他ロックに注意 | ◎ psql, DBeaver, WebUI | **◎ アプリ動作中にpsqlやDBeaverで即接続** |
| **暗号化・セキュリティ** | 有料 (SEE) / SQLCipher | ◎ 標準AES内蔵 | **◎ ChaCha20-Poly1305 / AES-GCM 内蔵** |

---

## 3. コアバリュー & 設計原則

### 原則 1: Zero-Config, Single-Dependency (SQLiteの美徳を継承)
- 初期設定ファイル、外部デーモン、サービス登録は一切不要。
- `h2::Connection::open("data.db")?` や `h2::Connection::open_in_memory()?` の1行で即座に動作。
- データベースは単一のディレクトリまたはファイル（`.h2`）として完結。

### 原則 2: Concurrent-Write First (SQLiteの最大の弱点を克服)
- H2のMVStore方式によるCopy-on-Write B-TreeとMVCCにより、**読み取りは書き込みをブロックせず、書き込みも読み取りをブロックしない**。
- スナップショット分離（Snapshot Isolation）をベースとし、非競合トランザクションの並行コミットを可能にする。

### 原則 3: Modern Data Native (AI時代・Web開発時代の組み込みDB)
- **JSON / JSONB**: ドキュメント指向のネストしたデータもそのまま格納・インデックス可能。
- **Vector Embeddings**: LLMの埋め込みベクトル（`VECTOR(1536)` 等）とコサイン類似度検索を標準サポートし、RAG（検索拡張生成）ローカルアプリを1クレートで実現。
- **フルテキスト検索 (FTS)**: 外部依存なしでテキストインデックス・検索が可能。

### 原則 4: Dual Personality: In-Process & PG-Wire (開発・運用の体験革新)
- アプリケーション内では高速なインプロセス関数呼び出し（IPCオーバーヘッドゼロ）。
- 同時に、オプションでバックグラウンドにて PostgreSQL互換TCPポート（例: `127.0.0.1:5432`）を開放可能。
- 開発中や運用中に、アプリを停止させることなく **DBeaver, DataGrip, VS Code拡張, psql** から直接クエリを発行して内部状態を確認・修正可能。
