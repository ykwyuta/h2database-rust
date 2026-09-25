#!/bin/bash
set -e

echo "================================================================================"
echo "  H2 Database Rust - 専用CLI デモ 3: トランザクショナル・キューテーブル"
echo "================================================================================"

DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" >/dev/null 2>&1 && pwd)"
echo "Executing demo script via h2-cli..."
cargo run -p h2-cli -- -f "$DIR/script.sql"
