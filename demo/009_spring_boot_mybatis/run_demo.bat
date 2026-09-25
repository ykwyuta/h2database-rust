@echo off
chcp 65001 > nul
echo ================================================================================
echo   H2 Database Rust - Spring Boot 4.1 / MyBatis (XML) Demo Launcher
echo ================================================================================

echo [1/2] Building Spring Boot application...
call mvn clean package -DskipTests
if %ERRORLEVEL% NEQ 0 (
    echo [ERROR] Maven build failed!
    exit /b %ERRORLEVEL%
)

echo [2/2] Running Spring Boot demo application...
echo Ensure demo-spring-boot-server is running on ports 5432 and 5433!
echo (You can run: cargo run -p demo-spring-boot-server)
call mvn spring-boot:run
