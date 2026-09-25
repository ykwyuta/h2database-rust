#!/bin/bash
set -e
cd "$(dirname "$0")"

echo "======================================================================"
echo "  H2 Database Rust - Spring JMS (JmsTemplate & @JmsListener) Demo"
echo "======================================================================"

echo "Checking if H2 PostgreSQL server is running on port 5432..."
if ! nc -z 127.0.0.1 5432 2>/dev/null && ! (echo > /dev/tcp/127.0.0.1/5432) 2>/dev/null; then
    echo "[ERROR] H2 server is not running on 127.0.0.1:5432."
    echo "Please start the server in another terminal:"
    echo "  cargo run -p demo-spring-boot-server"
    exit 1
fi

echo "Starting Spring Boot JMS Patterns application..."
mvn spring-boot:run
