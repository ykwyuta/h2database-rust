#!/bin/bash
set -e
cd "$(dirname "$0")"

echo "Building h2-cli..."
cargo build -p h2-cli --quiet

echo ""
echo "Running Demo 014: Authentication, Host Restrictions and Table Authorization..."
echo ""

../../target/debug/h2-cli -f script.sql
