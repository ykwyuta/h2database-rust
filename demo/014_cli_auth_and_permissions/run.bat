@echo off
setlocal
cd /d "%~dp0"

echo Building h2-cli...
cargo build -p h2-cli --quiet
if errorlevel 1 (
    echo Failed to build h2-cli
    exit /b 1
)

echo.
echo Running Demo 014: Authentication, Host Restrictions and Table Authorization...
echo.

..\..\target\debug\h2-cli.exe -f script.sql

endlocal
