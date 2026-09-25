@echo off
setlocal
cd /d "%~dp0"

echo ======================================================================
echo   H2 Database Rust - Spring JMS (JmsTemplate ^& @JmsListener) Demo
echo ======================================================================

echo Checking if H2 PostgreSQL server is running on port 5432...
powershell -Command "$tcp = New-Object Net.Sockets.TcpClient; try { $tcp.Connect('127.0.0.1', 5432); exit 0 } catch { exit 1 }"
if errorlevel 1 (
    echo [ERROR] H2 server is not running on 127.0.0.1:5432.
    echo Please start the server in another terminal:
    echo   cargo run -p demo-spring-boot-server
    exit /b 1
)

echo Starting Spring Boot JMS Patterns application...
call mvn spring-boot:run

endlocal
