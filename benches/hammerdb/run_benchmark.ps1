param(
    [int]$NumVU = 2,
    [int]$RampupMin = 1,
    [int]$DurationMin = 2,
    [string]$TargetHost = "postgres18",
    [int]$TargetPort = 5432
)

$ErrorActionPreference = "Stop"
$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
Set-Location $ScriptDir

Write-Host "==================================================" -ForegroundColor Cyan
Write-Host "  HammerDB TPROC-C Automated Benchmark Suite" -ForegroundColor Cyan
Write-Host "==================================================" -ForegroundColor Cyan

Write-Host "[1/4] Starting Docker services..." -ForegroundColor Green
docker compose up -d

Write-Host "[2/4] Waiting for database readiness..." -ForegroundColor Green
docker compose exec postgres18 pg_isready -U postgres -d postgres

Write-Host "[3/4] Building TPROC-C schema..." -ForegroundColor Green
docker compose exec -e PG_HOST=$TargetHost -e PG_PORT=$TargetPort hammerdb ./hammerdbcli auto /work/build_schema_pg.tcl

Write-Host "[4/4] Executing TPROC-C benchmark (VUs: $NumVU, Rampup: ${RampupMin}m, Duration: ${DurationMin}m)..." -ForegroundColor Green
docker compose exec `
  -e PG_HOST=$TargetHost `
  -e PG_PORT=$TargetPort `
  -e NUM_VU=$NumVU `
  -e RAMPUP_MIN=$RampupMin `
  -e DURATION_MIN=$DurationMin `
  hammerdb ./hammerdbcli auto /work/run_tprocc_pg.tcl

Write-Host "==================================================" -ForegroundColor Cyan
Write-Host "  HammerDB Benchmark Complete" -ForegroundColor Cyan
Write-Host "==================================================" -ForegroundColor Cyan
