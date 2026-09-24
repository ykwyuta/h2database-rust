# 06. 実装ロードマップとマイルストーン

データベース開発は非常に広範かつ複雑なため、小さく動作確認を重ねながら段階的に発展させるアプローチを取ります。

---

## 開発フェーズ一覧

```text
Phase 1: コアMVStore・型・基本ストレージエンジン (Foundation)
   │
   ▼
Phase 2: カタログ・最小SQL実行系・インメモリ/単一ファイル (Minimal RDBMS)
   │
   ▼
Phase 3: MVCCトランザクション・スナップショット分離・rusqlite風組み込みAPI (Embedded DB)
   │
   ▼
Phase 4: 内蔵PG-Wireサーバー・DBeaver/psql接続・インデックス拡充 (Server & Ecosystem)
   │
   ▼
Phase 5: 高機能化 (ベクトル検索, JSONB, CTE, Window関数, コンパクション) (Advanced Features)
```

---

## 各フェーズの詳細

### Phase 1: コアMVStore・型・基本ストレージエンジン (Foundation)
- **目標**: ログ構造化CoW B-Treeによる安全な永続化KVストアの完成。
- **タスク**:
  - `h2-types`: `DataType`, `Value` (Primitive, String, Binary), エラー型の定義。
  - `h2-mvstore`:
    - ファイルヘッダ、チャンク（Chunk）、スロット付きページ（Page）のバイナリレイアウト。
    - CoW B-Tree（挿入、探索、削除、範囲スキャン）。
    - 追記書き込みとチェックサム検証（xxHash/CRC32）。
    - クラッシュセーフなコミット（ルートポインタ更新）。
- **成果物**: 単体テストでランダムなPut/Get/Scanおよびクラッシュ・再起動後の完全性検証がパスすること。

### Phase 2: カタログ・最小SQL実行系 (Minimal RDBMS)
- **目標**: 簡単な `CREATE TABLE`, `INSERT`, `SELECT`, `WHERE` が動く最小限のリレーショナルDB。
- **タスク**:
  - `sqlparser-rs` の組み込みとAST変換。
  - カタログシステム（テーブル名、列定義、メタデータ保持）。
  - テーブルデータをMVStoreの行キー（`RowId -> RowData`）にマッピング。
  - Volcano型エグゼキュータ（`SeqScan`, `Filter`, `Project`）。
  - B-Treeセカンダリインデックス（`IndexScan`）。
- **成果物**: メモリ上およびファイル上で基本的なSQLが実行できること。

### Phase 3: MVCCトランザクション・スナップショット分離・rusqlite風API (Embedded DB)
- **目標**: SQLiteのようにRustアプリから呼び出せ、かつ複数リーダー・ライターが安全に並行動作する組み込みDBの完成。
- **タスク**:
  - `VersionedValue` と `TransactionStore` によるMVCC。
  - `BEGIN`, `COMMIT`, `ROLLBACK`（UNDOログ巻き戻し）。
  - スナップショット分離（Snapshot Isolation）によるロックフリーリード。
  - ファサードクレート `h2`:
    - `Connection::open()`, `Connection::open_in_memory()`.
    - `conn.execute()`, `conn.prepare()`, `stmt.query_map()`, `conn.transaction()`.
- **成果物**: `rusqlite` のサンプルコードとほぼ同一のコードで動作し、並行性テストでデッドロックや不整合が起きないこと。

### Phase 4: 内蔵PG-Wireサーバー・DBeaver/psql接続・結合 (Server & Ecosystem)
- **目標**: アプリケーションを動かしたまま、外部のpsqlやDBeaverから接続してSQLを実行できるハイブリッド環境の実現。
- **タスク**:
  - `h2-server`:
    - TokioによるTCPソケット待機、PostgreSQL v3プロトコルデコーダ/エンコーダ。
    - Simple Query Protocol（`'Q'`）、Extended Query Protocol（`'P'`, `'B'`, `'E'`, `'S'`）。
    - `INFORMATION_SCHEMA` と `pg_catalog` の主要ビュー（DBeaver等のメタデータ取得用）。
  - SQL機能拡張: `INNER JOIN`, `LEFT JOIN` (HashJoin / NestedLoopJoin)。
  - 集約関数: `COUNT`, `SUM`, `AVG`, `MIN`, `MAX`, `GROUP BY`, `ORDER BY`, `LIMIT`.
- **成果物**: `psql` CLIおよび `DBeaver` から接続し、テーブル一覧の表示、データの挿入・抽出・更新がグラフィカルに行えること。

### Phase 5: 高機能化 (AIベクトル検索, JSONB, 高度SQL, コンパクション) (Advanced Features)
- **目標**: SQLiteを凌駕する次世代高機能データベースとしての完成。
- **タスク**:
  - **ベクトル検索 (Vector)**: `VECTOR(dim)` 型、Cosine類似度/内積計算、HNSWインデックス。
  - **JSONB**: バイナリJSON形式、JSON抽出オペレータ（`->`, `->>`）。
  - **高度SQL**: `WITH RECURSIVE` (CTE)、Window関数（`ROW_NUMBER()`, `RANK()` 等）、生成列（Generated Column）。
  - **ストレージ最適化**:
    - バックグラウンド・チャンクリライト（GC / Vacuum）。
    - ページ圧縮（LZ4 / ZSTD）。
    - 透過的暗号化（ChaCha20-Poly1305 / AES-256-GCM）。
- **成果物**: RAG（AIローカル検索）アプリのデモ、sqllogictestの包括的合格。

---

## テスト・検証戦略

1. **Unit & Property-based Testing**:
   - `proptest` を用いたランダムなキー挿入・削除・更新によるB-Treeの不変条件（Invariants）検証。
2. **sqllogictest**:
   - SQLiteやPostgreSQLの標準テストスイート互換テストランナーを実行し、SQLの計算結果の正確性を保証。
3. **Crash Consistency Test**:
   - ランダムな書き込み途中で `std::process::exit(1)` や強制終了シグナルを発生させ、再起動後にデータ破損（Corruption）がないか自動検証。
4. **並行性・Jepsen風テスト**:
   - 多数のスレッドから並行して振替トランザクション（Alice -> Bob）を実行し、総残高が常に一致する（Invariant Preservation）ことの検証。
