# HammerDB TPROC-C Benchmark Suite (Docker)

本ディレクトリは、HammerDB v6.0 の公式 Docker イメージを活用し、PostgreSQL および `h2database-rust` に対して TPC-C 互換の TPROC-C 負荷検証を行うための自動化環境です。

## 構成ファイル

- [`docker-compose.yml`](file:///d:/workspace/h2database-rust/benches/hammerdb/docker-compose.yml): HammerDB クライアント (`tpcorg/hammerdb:latest`) および PostgreSQL 18 コンテナの定義
- [`build_schema_pg.tcl`](file:///d:/workspace/h2database-rust/benches/hammerdb/build_schema_pg.tcl): TPROC-C スキーマ（テーブル・インデックス・ストアドプロシージャ）自動構築スクリプト
- [`run_tprocc_pg.tcl`](file:///d:/workspace/h2database-rust/benches/hammerdb/run_tprocc_pg.tcl): TPROC-C 負荷計測自動実行スクリプト（Timed 測定、NOPM/TPM・レスポンスタイム集計）
- [`run_benchmark.sh`](file:///d:/workspace/h2database-rust/benches/hammerdb/run_benchmark.sh): Linux/macOS 向け自動実行スクリプト
- [`run_benchmark.ps1`](file:///d:/workspace/h2database-rust/benches/hammerdb/run_benchmark.ps1): Windows PowerShell 向け自動実行スクリプト

## クイックスタート

### 1. ベンチマークの実行

```bash
# Windows (PowerShell)
.\run_benchmark.ps1 -NumVU 2 -RampupMin 1 -DurationMin 2

# Linux / macOS
./run_benchmark.sh 2 1 2
```

### 2. 環境の停止

```bash
docker compose down
```

詳細な検証結果および考察については [docs/23_hammerdb_tprocc_performance_evaluation.md](file:///d:/workspace/h2database-rust/docs/23_hammerdb_tprocc_performance_evaluation.md) を参照してください。
