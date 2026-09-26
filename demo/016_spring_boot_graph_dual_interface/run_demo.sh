#!/usr/bin/env bash
set -e
cd "$(dirname "$0")"

echo "======================================================================"
echo "  H2 Database Rust - Spring Boot Graph DB Dual Interface Demo"
echo "======================================================================"

echo "Checking if H2 Dual-Protocol Server is running..."
echo "[1/2] Checking PostgreSQL Wire (SQL) on port 5432..."
if ! nc -z 127.0.0.1 5432 2>/dev/null && ! (echo > /dev/tcp/127.0.0.1/5432) 2>/dev/null; then
    echo "[ERROR] H2 SQL server is not running on 127.0.0.1:5432."
    echo "Please start the server in another terminal:"
    echo "  cargo run -p demo-graph-server"
    exit 1
fi

echo "[2/2] Checking Neo4j Bolt (Cypher) on port 7687..."
if ! nc -z 127.0.0.1 7687 2>/dev/null && ! (echo > /dev/tcp/127.0.0.1/7687) 2>/dev/null; then
    echo "[ERROR] H2 Bolt server is not running on 127.0.0.1:7687."
    echo "Please start the server in another terminal:"
    echo "  cargo run -p demo-graph-server"
    exit 1
fi

echo "[OK] Both servers are active. Starting Spring Boot Graph application..."
mvn spring-boot:run
