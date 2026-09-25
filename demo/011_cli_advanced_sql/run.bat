@echo off
chcp 65001 > nul
echo ================================================================================
echo   H2 Database Rust - 専用CLI デモ 1: 高度な SQL & クエリ演算
echo ================================================================================

echo Executing demo script via h2-cli...
cargo run -p h2-cli -- -f "%~dp0script.sql"
