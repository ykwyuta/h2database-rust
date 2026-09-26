# 高速バイナリバックアップ・PITR (Point-in-Time Recovery)・リードレプリカバックアップ設計方針書

## 1. 概要と背景課題

### 1.1 背景と現行の課題
現行の `h2database-rust` におけるバックアップおよび復元機能（`crates/h2-mvstore/src/store.rs` の `dump_backup` / `restore_backup`）は、全マップのキーとバリューの生バイト列（`Vec<u8>`）を `serde_json::to_vec_pretty` で直列化して出力する実装となっていました。

運用レビュー（[`docs/review/operation/README.md`](file:///d:/workspace/h2database-rust/docs/review/operation/README.md)）および実運用において、以下の重大な課題が指摘されています:

1. **JSON フォーマットの性能・容量ペナルティ**:
   - `Vec<u8>` の各バイトが JSON の数値配列 `[120, 23, 14, ...]` としてテキスト化されるため、データ容量が元のバイナリサイズに対して **4〜10 倍に肥大化**。
   - JSON のパース・文字列変換コストにより、ギガバイト級データにおけるバックアップ/リストア速度が著しく低下。
2. **整合性・検証性の欠如**:
   - バックアップファイル内にスナップショット境界バージョン、形式バージョン、チェックサム、完了マニフェストが存在しない。
   - バックアップ破損時の事前検出手段（SQL Server の `RESTORE VERIFYONLY` や Oracle RMAN の `RESTORE VALIDATE` 相当）がない。
3. **PITR (Point-in-Time Recovery / 任意時点復旧) の未対応**:
   - ベースバックアップと継続 WAL アーカイブを組み合わせて、「障害発生直前の 14:02:15 時点」や「誤操作直前のトランザクション」へデータベースを巻き戻す機能が未提供。
4. **リードレプリカからのバックアップ取得未対応**:
   - 本番プライマリのトランザクションスループットに負荷をかけず、読み取り専用スタンバイ（リードレプリカ）から無停止で整合バックアップを取得する仕組みが必要。

---

## 2. アーキテクチャ全体像

```
┌────────────────────────────────────────────────────────────────────────┐
│                        Primary / Replica Instance                      │
│                                                                        │
│   ┌───────────────────────────┐      ┌─────────────────────────────┐   │
│   │ MVStore In-Memory Maps    │      │ WalManager & Continuous     │   │
│   │ (Versioned Trees)         │      │ WalArchiver                 │   │
│   └─────────────┬─────────────┘      └──────────────┬──────────────┘   │
└─────────────────┼───────────────────────────────────┼──────────────────┘
                  │ (Snapshot Read Lock)              │ (Continuous Archive)
                  ▼                                   ▼
      ┌───────────────────────┐           ┌───────────────────────┐
      │ Binary Backup File    │           │ WAL Archive Directory │
      │ (*.h2bk)              │           │ (wal_*.wal)           │
      │                       │           │                       │
      │ - Magic: 'H2BK'       │           │ - Segment 0001        │
      │ - Header + Version    │           │ - Segment 0002        │
      │ - Map Entries (Binary)│           │ - Segment 0003...     │
      │ - CRC32 + Manifest    │           │                       │
      └───────────┬───────────┘           └───────────┬───────────┘
                  │                                   │
                  └─────────────────┬─────────────────┘
                                    │
                                    ▼
                      ┌───────────────────────────┐
                      │ PITR Restore Engine       │
                      │                           │
                      │ 1. Restore Base Backup    │
                      │ 2. Scan WAL Archive       │
                      │ 3. Replay up to Target    │
                      │    (Timestamp / Version)  │
                      │ 4. Atomic Commit & Verify │
                      └───────────────────────────┘
```

---

## 3. 高速バイナリバックアップフォーマット仕様 (`*.h2bk`)

従来の JSON 配列に代わり、ゼロアロケーション走査と高速デシリアライズが可能なストリーミングバイナリフォーマットを採用します。

### 3.1 フォーマット構造

```
+-------------------------------------------------------------------------+
| FILE HEADER (64 bytes 固定長)                                           |
|  - Magic: b"H2BK" (4 bytes)                                             |
|  - Format Version: u32 (1)                                              |
|  - Backup ID: [u8; 16] (UUID)                                           |
|  - Snapshot Version: u64                                                |
|  - Timestamp Nanos: i64 (UNIX epoch)                                    |
|  - Source Role: u8 (0 = Primary, 1 = Standby Replica)                    |
|  - Checksum Type: u8 (1 = CRC32Fast)                                    |
|  - Total Maps: u32                                                      |
|  - Header Checksum: u32 (CRC32 of preceding 60 bytes)                   |
+-------------------------------------------------------------------------+
| MAP SECTION (可変長, マップ数分繰り返し)                                 |
|  - Map Name Length: u32                                                 |
|  - Map Name Bytes: [u8; len] (UTF-8)                                    |
|  - Entry Count: u64                                                     |
|  - Map Entries:                                                         |
|     - Key Length: u32, Key Bytes: [u8; key_len]                         |
|     - Val Length: u32, Val Bytes: [u8; val_len]                         |
|  - Map CRC32: u32                                                       |
+-------------------------------------------------------------------------+
| TRAILER / MANIFEST (24 bytes 固定長)                                    |
|  - Total Data Bytes: u64                                                |
|  - Overall Payload Checksum: u32 (CRC32 of all maps)                    |
|  - Total Records: u64                                                   |
|  - End Magic: b"H2EF" (4 bytes)                                         |
+-------------------------------------------------------------------------+
```

### 3.2 期待される効果
- **ファイルサイズ**: JSON 配列形式と比べ **75%〜85% のサイズ削減**。
- **JSON バックアップの完全廃止**: 性能および整合性リスク（型安全性欠如、肥大化）の高い JSON ダンプは完全廃止され、`H2BK` バイナリフォーマットに一元化。非バイナリファイルは安全に拒否。

---

## 4. 継続 WAL アーカイブと PITR (Point-in-Time Recovery)

### 4.1 WAL アーカイバ (`WalArchiver`)
トランザクションのコミットログ（WAL）をバックアップストレージや外部アーカイブディレクトリに安全に退避する機構です。

1. **アーカイブディレクトリ構造**:
   ```
   <archive_dir>/
     ├── wal_meta.json              (メタデータ: 最新アーカイブバージョン、タイムスタンプ)
     ├── wal_0000000000000001.wal  (バージョン 1 からの WAL セグメント)
     ├── wal_0000000000000100.wal  (バージョン 100 からの WAL セグメント)
     └── ...
   ```
2. **セグメントローテーション**:
   - 指定サイズ（デフォルト 16MB）または時間間隔で WAL セグメントを切り替え。
   - チェックサム検証済みのセグメントのみをコミット済みとしてアーカイブに永続化。

### 4.2 リカバリターゲット (`RecoveryTarget`)
復旧対象の時点を指定する柔軟なターゲット列挙型を定義します:

```rust
pub enum RecoveryTarget {
    /// 指定した時刻（UTC / タイムスタンプ）直前の状態まで復元
    Timestamp(chrono::DateTime<chrono::Utc>),
    /// 指定したコミットバージョン直前の状態まで復元
    Version(u64),
    /// 指定したトランザクション ID 完了時点まで復元
    TransactionId(u64),
    /// 利用可能な最新のアーカイブ WAL まで完全にロールフォワード
    Latest,
}
```

### 4.3 PITR 実行アルゴリズム
1. **ベースバックアップの適用**:
   - 指定されたベースバックアップファイル（`*.h2bk`）を読み込み、スナップショットバージョン $V_{base}$、タイムスタンプ $T_{base}$ へ復元。
2. **WAL アーカイブの順序走査**:
   - `archive_dir` 内の全 WAL セグメントをソートし、$V_{base}$ 以降のレコードを抽出。
3. **ターゲット判定とロールフォワード**:
   - 各 `WalRecord` について:
     - `RecoveryTarget::Timestamp(t)`: レコードの作成時刻 $\le t$ の場合のみ適用。
     - `RecoveryTarget::Version(v)`: `record.commit_version <= v` の場合のみ適用。
   - 対象レコードの `changes`（INSERT, UPDATE, DELETE）を対象マップへ順番に適用。
4. **整合性チェックとアトミックコミット**:
   - 目標地点に到達した時点で再生を完了し、新規バージョンとしてコミット。
   - 復旧レポート（適用レコード数、到達バージョン、到達時刻）を返却。

---

## 5. リードレプリカ（Standby）からのバックアップ取得機構

プライマリの書き込み負荷を軽減するため、レプリケーションスタンバイ（リードレプリカ）から整合スナップショットを取得できるようにします。

### 5.1 スタンバイバックアップの要件
1. **読み取り専用ガードの保持**:
   - スタンバイインスタンスは `is_read_only = true` で動作しており、バックアップ取得中も書き込みを受け付けない。
2. **適用中スナップショットの整合性**:
   - スタンバイがプライマリからのストリーミング WAL を適用している最中にバックアップを開始する場合、`checkpoint_gate` またはマップ読み取りロックを取得し、途中の未コミット変更を含まない完全なトランザクション境界でダンプ。
3. **メタデータへのレプリカ情報記録**:
   - バックアップヘッダに `source_role = StandbyReplica`、およびレプリカが同期済みの `commit_version` を記録。
   - このバックアップファイルをベースバックアップとして、プライマリ側の継続 WAL アーカイブと組み合わせて PITR 復元可能。

---

## 6. インターフェース設計

### 6.1 Rust 組み込み API
```rust
impl Connection {
    /// 高速バイナリ形式でデータベースをバックアップ
    pub fn backup<P: AsRef<Path>>(&self, path: P) -> H2Result<BackupMetadata>;

    /// バックアップファイルの整合性を検証（復元は行わない: VERIFYONLY 相当）
    pub fn verify_backup<P: AsRef<Path>>(&self, path: P) -> H2Result<BackupMetadata>;

    /// ベースバックアップと WAL アーカイブを用いた PITR 復元
    pub fn restore_pitr<P: AsRef<Path>, A: AsRef<Path>>(
        &self,
        backup_path: P,
        archive_dir: Option<A>,
        target: RecoveryTarget,
    ) -> H2Result<RestoreReport>;
}

impl Instance {
    /// プライマリまたはレプリカから整合バックアップを取得
    pub fn backup<P: AsRef<Path>>(&self, path: P) -> H2Result<BackupMetadata>;
}
```

### 6.2 SQL インターフェース
```sql
-- 1. バイナリバックアップの作成
BACKUP TO '/path/to/backup.h2bk';

-- 2. バックアップの検証のみ実行 (データは書き換えない)
RESTORE VERIFYONLY FROM '/path/to/backup.h2bk';

-- 3. ベースバックアップからの通常復元
RESTORE FROM '/path/to/backup.h2bk';

-- 4. WAL アーカイブを併用した任意時点復旧 (PITR)
RESTORE FROM '/path/to/backup.h2bk'
  WITH WAL_ARCHIVE = '/path/to/wal_archive'
  RECOVERY_TARGET_TIME = '2026-09-26 18:00:00';
```

---

## 7. 実装ステップ計画

1. **フェーズ 1: 高速バイナリバックアップフォーマットの実装**
   - `crates/h2-mvstore/src/backup.rs` の新設: `write_binary_backup`, `verify_binary_backup`, `read_and_verify_binary_backup`, ヘッダ・トレイラー定義、CRC32 検証。
   - `MVStore::dump_backup` / `restore_backup` をバイナリ形式に刷新（レガシー JSON は完全廃止・安全に遮断）。
   - `verify_backup` の実装（データ非破壊でのチェックサム・整合性チェック）。

2. **フェーズ 2: 継続 WAL アーカイブと PITR エンジンの実装**
   - `crates/h2-mvstore/src/wal.rs` に `WalArchiver` と `RecoveryTarget` を追加。
   - コミット時の WAL アーカイブ出力機構の実装。
   - ベースバックアップ復元 + WAL アーカイブ再生による PITR ロールフォワードエンジンの実装。

3. **フェーズ 3: リードレプリカバックアップの統合**
   - `crates/h2/src/replication.rs` の `Instance` に `backup(&self, path)` を追加。
   - スタンバイインスタンス（Read-Only）からの整合バックアップ取得と、プライマリへの影響ゼロをテスト。

4. **フェーズ 4: SQL および API レイヤの拡張と総合検証**
   - `crates/h2-sql` の `BACKUP TO` / `RESTORE FROM` / `RESTORE VERIFYONLY` 構文の拡張。
   - 破損バックアップの遮断テスト、PITR による誤 DROP TABLE からの特定時刻復元テスト、レプリカバックアップ復元テストの完備。
