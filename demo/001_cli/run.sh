#!/usr/bin/env bash
set -e
echo "========================================================"
echo "H2 Database in Rust - Dedicated Interactive CLI"
echo "========================================================"
echo
echo "Launching CLI with database file: demo.h2"
echo
cargo run --manifest-path ../../Cargo.toml -p h2-cli -- demo.h2
