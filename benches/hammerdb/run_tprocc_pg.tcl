#!/bin/tclsh
puts "=================================================="
puts "  HammerDB TPROC-C: Workload Benchmark Run"
puts "=================================================="

dbset db pg
dbset bm TPC-C

set host     [expr {[info exists env(PG_HOST)] ? $env(PG_HOST) : "postgres18"}]
set port     [expr {[info exists env(PG_PORT)] ? $env(PG_PORT) : 5432}]
set vu       [expr {[info exists env(NUM_VU)] ? $env(NUM_VU) : 4}]
set rampup   [expr {[info exists env(RAMPUP_MIN)] ? $env(RAMPUP_MIN) : 1}]
set duration [expr {[info exists env(DURATION_MIN)] ? $env(DURATION_MIN) : 2}]

puts "Configuration:"
puts "  Target Host:       $host:$port"
puts "  Virtual Users:     $vu"
puts "  Rampup (minutes):  $rampup"
puts "  Duration (minutes):$duration"
puts "--------------------------------------------------"

diset connection pg_host $host
diset connection pg_port $port
diset connection pg_sslmode prefer

diset tpcc pg_superuser postgres
diset tpcc pg_superuserpass postgres
diset tpcc pg_defaultdbase postgres
diset tpcc pg_user tpcc
diset tpcc pg_pass tpcc
diset tpcc pg_dbase tpcc

diset tpcc pg_driver timed
diset tpcc pg_rampup $rampup
diset tpcc pg_duration $duration
diset tpcc pg_vacuum false
diset tpcc pg_timeprofile true
diset tpcc pg_allwarehouse true

loadscript
puts "Virtual Users: Setting $vu VUs"
vuset vu $vu
vucreate
tcstart
tcstatus

puts "Starting TPROC-C test run..."
set jobid [ vurun ]

vudestroy
tcstop
puts "--------------------------------------------------"
puts "TEST COMPLETED. Job ID: $jobid"
puts "--------------------------------------------------"

puts "=== TRANSACTION SUMMARY ==="
catch { job $jobid tcount }
puts ""
puts "=== RESPONSE TIME PROFILE ==="
catch { job $jobid timing }
puts ""
puts "=== HAMMERDB PERFORMANCE RESULT ==="
catch { job $jobid result }
puts ""
puts "=================================================="
