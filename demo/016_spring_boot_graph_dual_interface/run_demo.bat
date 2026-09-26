@echo off
setlocal
cd /d "%~dp0"

echo ======================================================================
echo   H2 Database Rust - Spring Boot Graph DB Dual Interface Demo
echo ======================================================================

echo Checking if H2 Dual-Protocol Server is running...
echo [1/2] Checking PostgreSQL Wire (SQL) on port 5432...
powershell -Command "$tcp = New-Object Net.Sockets.TcpClient; try { $tcp.Connect('127.0.0.1', 5432); exit 0 } catch { exit 1 }"
if errorlevel 1 (
    echo [ERROR] H2 SQL server is not running on 127.0.0.1:5432.
    echo Please start the server in another terminal:
    echo   cargo run -p demo-graph-server
    exit /b 1
)

echo [2/2] Checking Neo4j Bolt (Cypher) on port 7687...
powershell -Command "$tcp = New-Object Net.Sockets.TcpClient; try { $tcp.Connect('127.0.0.1', 7687); exit 0 } catch { exit 1 }"
if errorlevel 1 (
    echo [ERROR] H2 Bolt server is not running on 127.0.0.1:7687.
    echo Please start the server in another terminal:
    echo   cargo run -p demo-graph-server
    exit /b 1
)

echo [OK] Both servers are active. Starting Spring Boot Graph application...
call mvn spring-boot:run

endlocal
