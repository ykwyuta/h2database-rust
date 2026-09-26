#!/bin/tclsh
puts "=================================================="
puts "  HammerDB TPROC-C: Building PostgreSQL Schema"
puts "=================================================="

dbset db pg
dbset bm TPC-C

set host [expr {[info exists env(PG_HOST)] ? $env(PG_HOST) : "postgres18"}]
set port [expr {[info exists env(PG_PORT)] ? $env(PG_PORT) : 5432}]
set ware [expr {[info exists env(PG_WAREHOUSE)] ? $env(PG_WAREHOUSE) : 1}]
set vu   [expr {[info exists env(PG_BUILD_VU)] ? $env(PG_BUILD_VU) : 1}]

puts "Target Host: $host:$port"
puts "Warehouses:  $ware"
puts "VirtualUsers:$vu"

diset connection pg_host $host
diset connection pg_port $port
diset connection pg_sslmode prefer

diset tpcc pg_count_ware $ware
diset tpcc pg_num_vu $vu
diset tpcc pg_superuser postgres
diset tpcc pg_superuserpass postgres
diset tpcc pg_defaultdbase postgres
diset tpcc pg_user tpcc
diset tpcc pg_pass tpcc
diset tpcc pg_dbase tpcc
diset tpcc pg_tspace pg_default
diset tpcc pg_storedprocs true
diset tpcc pg_partition false

puts "--------------------------------------------------"
puts "Starting buildschema..."
buildschema
puts "--------------------------------------------------"
puts "SCHEMA BUILD COMPLETED SUCCESSFULLY"
