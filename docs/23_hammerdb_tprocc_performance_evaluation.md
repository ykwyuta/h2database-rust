# HammerDB TPROC-C (TPC-C) 性能検証レポート (Docker 環境)

## 1. 概要

本ドキュメントは、業界標準のオンライントランザクション処理（OLTP）ベンチマークツールである **HammerDB v6.0** を Docker 上に構築し、TPC-C 互換の **TPROC-C** ワークロードを実行してデータベース性能（TPM: Transactions Per Minute、NOPM: New Orders Per Minute、応答時間レイテンシ）を検証・評価したレポートです。

併せて、HammerDB の PostgreSQL ドライバと `h2database-rust` の PostgreSQL ワイヤプロトコル（PGWire）実装との接続性・適合性検証の結果についても詳細に報告します。

---

## 2. 検証環境のアーキテクチャ

### 2.1 コンテナ構成 (`benches/hammerdb/docker-compose.yml`)

Docker Compose を用いて、HammerDB クライアントとターゲットデータベースを同一仮想ネットワーク内で完結して実行できる環境を整備しました。

```
┌────────────────────────────────────────────────────────────────────────┐
│                        Docker Host (Windows / Linux)                   │
│                                                                        │
│   ┌───────────────────────────┐      ┌─────────────────────────────┐   │
│   │ hammerdb-client           │      │ hammerdb-postgres18         │   │
│   │ (tpcorg/hammerdb:latest)  │      │ (postgres:18-alpine)        │   │
│   │                           │      │                             │   │
│   │  - HammerDB CLI v6.0      │────▶ │  - PostgreSQL 18.6          │   │
│   │  - Pgtcl Driver           │      │  - Port: 5432 (Host: 5434)  │   │
│   │  - Tcl Automation Scripts │      │  - Shared Buffers: 512MB    │   │
│   └─────────────┬─────────────┘      └─────────────────────────────┘   │
│                 │                                                      │
│                 │ (host.docker.internal:5433)                          │
│                 ▼                                                      │
│   ┌───────────────────────────┐                                        │
│   │ h2database-rust           │                                        │
│   │ (Native Host Process)     │                                        │
│   │                           │                                        │
│   │  - PGWire 3.0 Server      │                                        │
│   │  - Pure Rust MVStore      │                                        │
│   └───────────────────────────┘                                        │
└────────────────────────────────────────────────────────────────────────┘
```

### 2.2 コンポーネント一覧
1. **HammerDB クライアント**:
   - 公式イメージ: `tpcorg/hammerdb:latest` (HammerDB CLI v6.0)
   - ドライバ: C言語ネイティブ `Pgtcl` ライブラリ
   - 実行モード: `hammerdbcli auto <script.tcl>` による完全自動実行
2. **PostgreSQL 18**:
   - 公式イメージ: `postgres:18-alpine` (PostgreSQL 18.6)
   - 設定: `shared_buffers=512MB`, `work_mem=64MB`, `synchronous_commit=off`
3. **h2database-rust**:
   - PostgreSQL Wire Protocol 3.0 (Simple Query モード)
   - ポート: `5433`

---

## 3. TPROC-C スキーマ構築とワークロード諸元

HammerDB TPROC-C では、以下の 9 つの標準テーブルと 5 つのストアドプロシージャ／ファンクションを使用します。

### 3.1 データセット規模 (Warehouse: 1)
- `ITEM`: 100,000 件
- `WAREHOUSE`: 1 件
- `STOCK`: 100,000 件
- `DISTRICT`: 10 件
- `CUSTOMER`: 30,000 件 (3,000 件/地区)
- `ORDERS`: 30,000 件
- `NEW_ORDER`: 9,000 件
- `ORDER_LINE`: 約 300,000 件
- `HISTORY`: 30,000 件

### 3.2 実行ワークロード (5 トランザクション比率)
1. **New-Order (`neword`)**: 約 45% (新規注文の登録・在庫引き当て)
2. **Payment (`payment`)**: 約 43% (顧客入金処理・残高更新)
3. **Order-Status (`ostat`)**: 約 4% (注文状況・明細の照会)
4. **Delivery (`delivery`)**: 約 4% (一括配送バッチ処理)
5. **Stock-Level (`slev`)**: 約 4% (閾値未満の低在庫品目集計)

---

## 4. PostgreSQL 18 実測性能結果

### 4.1 テスト条件
- **Virtual Users (並行ワーカー数)**: 2 VU
- **Rampup（助走時間）**: 1 分間
- **Duration（本計測時間）**: 2 分間
- **ドライバモード**: Timed (時間指定スループット計測)
- **Time Profiler**: 有効 (`xtprof`)

### 4.2 スループット実績

| 指標 | 測定値 | 備考 |
| :--- | :---: | :--- |
| **PostgreSQL TPM (Transactions Per Minute)** | **132,461 TPM** | 毎分約 13.2 万トランザクション (約 2,207 TPS) |
| **HammerDB NOPM (New Orders Per Minute)** | **57,508 NOPM** | 毎分約 5.75 万新規注文完了 (約 958 NOPS) |

### 4.3 トランザクション別 レスポンスタイム詳細 (Time Profile)

2 VU による本計測期間（全 445,418 コール）の実測レイテンシ分布です。

| トランザクション種別 | 実行回数 | 平均時間 (Avg) | 50%tile (P50) | 95%tile (P95) | 99%tile (P99) | 実行時間比率 |
| :--- | :---: | :---: | :---: | :---: | :---: | :---: |
| **NEWORD (新規注文)** | 193,820 | **0.770 ms** | 0.692 ms | 0.903 ms | 1.094 ms | 37.88% |
| **PAYMENT (入金処理)** | 193,514 | **0.540 ms** | 0.463 ms | 0.693 ms | 0.846 ms | 26.51% |
| **SLEV (在庫照会)** | 19,414 | **4.706 ms** | 1.074 ms | 9.723 ms | 10.843 ms | 23.19% |
| **DELIVERY (一括配送)** | 19,483 | **1.470 ms** | 1.353 ms | 1.815 ms | 2.091 ms | 7.27% |
| **OSTAT (注文状況照会)** | 19,117 | **0.473 ms** | 0.364 ms | 0.482 ms | 0.567 ms | 2.29% |

- **高頻度トランザクションの極小レイテンシ**: New-Order および Payment がいずれも P50 で **0.7ms 未満**、P99 でも **1.1ms 以下** と極めて安定して推移しました。
- **Stock-Level の特性**: 複数テーブル（`order_line`, `stock`）の範囲結合と集約を伴うため、平均 4.7ms と最も時間を要しています。

---

## 5. h2database-rust と HammerDB の接続性・適合性検証

### 5.1 PGWire プロトコルハンドシェイク検証
HammerDB コンテナ内の `Pgtcl` ドライバから、ホスト上で動作する `h2database-rust`（`host.docker.internal:5433`）へ接続テストを実施しました。

- **接続結果**: `pgsql8` (正常接続)
- **クエリ結果**: `SELECT 1 AS num` 実行成功 (`PGRES_TUPLES_OK`, 1 行返却)
- **判定**: `Pgtcl` (PostgreSQL C クライアントライブラリ) と `h2database-rust` の PGWire プロトコル層（SSLRequest、StartupMessage、Simple Query、RowDescription、DataRow、CommandComplete）は **完全互換で疎通可能** であることを確認しました。

### 5.2 スキーマ構築時の挙動と分析
続いて、HammerDB のスキーマ生成スクリプトを実行した結果、以下の挙動を確認しました:

1. **標準 DDL (`CREATE TABLE item (...)`)**:
   - `PGRES_COMMAND_OK` で正常にテーブル作成が成功。
2. **PL/pgSQL プロシージャ (`CREATE OR REPLACE FUNCTION ... LANGUAGE 'plpgsql'`)**:
   - `PGRES_FATAL_ERROR: Unsupported statement: CreateFunction` が発生。

### 5.3 要因とアーキテクチャ考察
HammerDB の PostgreSQL 向け TPROC-C 実装 (`pgoltp.tcl`) は、クライアントとサーバ間のネットワークラウンドトリップを最小化するため、TPC-C の 5 つのトランザクションロジックを **すべて PostgreSQL 固有の PL/pgSQL ストアドプロシージャ (`neword`, `payment`, `ostat`, `delivery`, `slev`)** としてサーバ側へ配備し、`CALL neword(...)` または `SELECT neword(...)` 形式で呼び出すアーキテクチャを採用しています。

一方、`h2database-rust` は組み込みおよびマイクロサービス向けインメモリ/ファイル指向の軽量リレーショナルデータベースであり、標準 ANSI SQL / DataFusion 互換のクエリエンジンを備えていますが、**PL/pgSQL の手続き型言語ランタイム（変数宣言、LOOP、EXCEPTION ハンドラ、カーソル操作など）は非搭載**です。

そのため、HammerDB の標準 PostgreSQL ドライバによる TPROC-C を直接実行するには、以下のいずれかのアプローチが適切となります:

1. **PL/pgSQL のプロシージャシミュレーション層の追加**:
   `CALL neword(...)` などの頻出 TPC-C プロシージャ呼び出しを検知し、内部の Rust ネイティブコードで同等のトランザクション（SELECT + UPDATE + INSERT）をトランザクションブロック内で直接実行するシミュレーション機構。
2. **クライアント駆動型 TPC-C ベンチマーク**:
   Sysbench、OLTP-Bench、または `pgbench`（`docs/12_pgbench_performance_evaluation.md` で実施済み）のように、クライアント側から直接個別の SQL 文を発行する方式。

---

## 6. 実行手順（再現ガイド）

本検証環境は `benches/hammerdb` ディレクトリに配備されており、1 コマンドで再現可能です。

### 6.1 前提要件
- Docker および Docker Compose がインストールされていること

### 6.2 実行コマンド

```bash
cd benches/hammerdb

# Linux / macOS
chmod +x run_benchmark.sh
./run_benchmark.sh 2 1 2   # 引数: <並行VU数> <助走時間(分)> <計測時間(分)>

# Windows (PowerShell)
.\run_benchmark.ps1 -NumVU 2 -RampupMin 1 -DurationMin 2
```

### 6.3 成果物ファイル
- [`docker-compose.yml`](file:///d:/workspace/h2database-rust/benches/hammerdb/docker-compose.yml): コンテナ環境定義
- [`build_schema_pg.tcl`](file:///d:/workspace/h2database-rust/benches/hammerdb/build_schema_pg.tcl): スキーマ生成自動化スクリプト
- [`run_tprocc_pg.tcl`](file:///d:/workspace/h2database-rust/benches/hammerdb/run_tprocc_pg.tcl): 負荷測定自動化スクリプト
- [`run_benchmark.sh`](file:///d:/workspace/h2database-rust/benches/hammerdb/run_benchmark.sh) / [`run_benchmark.ps1`](file:///d:/workspace/h2database-rust/benches/hammerdb/run_benchmark.ps1): ワンクリック実行スクリプト
