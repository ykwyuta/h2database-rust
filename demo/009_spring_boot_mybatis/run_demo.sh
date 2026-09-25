#!/usr/bin/env bash
set -e

echo "================================================================================"
echo "  H2 Database Rust - Spring Boot 4.1 / MyBatis (XML) Demo Launcher"
echo "================================================================================"

echo "[1/2] Building Spring Boot application..."
mvn clean package -DskipTests

echo "[2/2] Running Spring Boot demo application..."
echo "Ensure demo-spring-boot-server is running on ports 5432 and 5433!"
echo "(You can run: cargo run -p demo-spring-boot-server)"
mvn spring-boot:run
