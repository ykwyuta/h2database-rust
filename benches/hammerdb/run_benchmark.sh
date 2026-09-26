#!/bin/bash
set -e

DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$DIR"

echo "=================================================="
echo "  HammerDB TPROC-C Automated Benchmark Suite"
echo "=================================================="

# 1. Start Docker containers
echo "[1/4] Starting Docker services..."
docker compose up -d

# 2. Wait for PostgreSQL
echo "[2/4] Waiting for PostgreSQL 18 to become healthy..."
docker compose exec postgres18 pg_isready -U postgres -d postgres

# 3. Build TPROC-C Schema
echo "[3/4] Building TPROC-C schema (1 Warehouse)..."
docker compose exec -e PG_HOST=postgres18 -e PG_PORT=5432 hammerdb ./hammerdbcli auto /work/build_schema_pg.tcl

# 4. Run TPROC-C Workload
NUM_VU=${1:-2}
RAMPUP_MIN=${2:-1}
DURATION_MIN=${3:-2}

echo "[4/4] Running TPROC-C benchmark (VUs: $NUM_VU, Rampup: ${RAMPUP_MIN}m, Duration: ${DURATION_MIN}m)..."
docker compose exec \
  -e PG_HOST=postgres18 \
  -e PG_PORT=5432 \
  -e NUM_VU=$NUM_VU \
  -e RAMPUP_MIN=$RAMPUP_MIN \
  -e DURATION_MIN=$DURATION_MIN \
  hammerdb ./hammerdbcli auto /work/run_tprocc_pg.tcl

echo "=================================================="
echo "  HammerDB Benchmark Complete"
echo "=================================================="
