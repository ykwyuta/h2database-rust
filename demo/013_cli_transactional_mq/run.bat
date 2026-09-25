@echo off
chcp 65001 > nul
echo ================================================================================
echo   H2 Database Rust - 専用CLI デモ 3: トランザクショナル・キューテーブル
echo ================================================================================

echo Executing demo script via h2-cli...
cargo run -p h2-cli -- -f "%~dp0script.sql"
