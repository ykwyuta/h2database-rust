@echo off
echo ========================================================
echo H2 Database in Rust - Dedicated Interactive CLI
echo ========================================================
echo.
echo Launching CLI with database file: demo.h2
echo (Leave empty or run in-memory with cargo run -p h2-cli)
echo.
cargo run --manifest-path ..\..\Cargo.toml -p h2-cli -- demo.h2
