@echo off
echo ========================================================
echo Running Java JDBC Demo against H2 Database in Rust
echo ========================================================
echo Ensure demo-psql-server is running on port 5432!
echo.
mvn compile exec:java
