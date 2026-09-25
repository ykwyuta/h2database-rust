#!/bin/bash
set -e

echo "================================================================================"
echo "  H2 Database Rust - Spring Boot 4.1 / Spring JMS Standard Demo Launcher"
echo "================================================================================"

echo "[1/2] Building Spring Boot application..."
mvn clean package -DskipTests

echo "[2/2] Running Spring Boot demo application..."
echo "Ensure demo-spring-boot-server is running on port 5432!"
echo "(You can run: cargo run -p demo-spring-boot-server)"
mvn spring-boot:run
