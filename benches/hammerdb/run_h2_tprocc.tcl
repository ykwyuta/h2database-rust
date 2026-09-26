#!/bin/tclsh
puts "=================================================="
puts "  HammerDB TPROC-C Workload on h2database-rust"
puts "  (via PL/pgSQL Simulation Layer)"
puts "=================================================="

package require Pgtcl

set host [expr {[info exists env(H2_HOST)] ? $env(H2_HOST) : "host.docker.internal"}]
set port [expr {[info exists env(H2_PORT)] ? $env(H2_PORT) : 5433}]
set iterations [expr {[info exists env(ITERATIONS)] ? $env(ITERATIONS) : 20000}]

puts "Target: $host:$port, Iterations: $iterations"
set lda [pg_connect -conninfo "host=$host port=$port dbname=mydb user=postgres"]
puts "Connected to h2database-rust."

puts "Starting TPROC-C transaction mix..."
set start_time [clock milliseconds]

set neword_cnt 0
set payment_cnt 0
set slev_cnt 0
set delivery_cnt 0
set ostat_cnt 0

for {set i 0} {$i < $iterations} {incr i} {
    set choice [expr {$i % 100}]
    if {$choice < 45} {
        # NEWORD (45%)
        set res [pg_exec $lda "CALL neword(1, 1, [expr {($i % 10) + 1}], [expr {($i % 3000) + 1}], 10, 0.0, '', '', 0.0, 0.0, 0, CURRENT_TIMESTAMP)"]
        pg_result $res -clear
        incr neword_cnt
    } elseif {$choice < 88} {
        # PAYMENT (43%)
        set res [pg_exec $lda "CALL payment(1, [expr {($i % 10) + 1}], 1, [expr {($i % 10) + 1}], [expr {($i % 3000) + 1}], 0, 50.0, 'BAR')"]
        pg_result $res -clear
        incr payment_cnt
    } elseif {$choice < 92} {
        # SLEV (4%)
        set res [pg_exec $lda "CALL slev(1, [expr {($i % 10) + 1}], 15)"]
        pg_result $res -clear
        incr slev_cnt
    } elseif {$choice < 96} {
        # DELIVERY (4%)
        set res [pg_exec $lda "CALL delivery(1, 1, CURRENT_TIMESTAMP)"]
        pg_result $res -clear
        incr delivery_cnt
    } else {
        # OSTAT (4%)
        set res [pg_exec $lda "CALL ostat(1, [expr {($i % 10) + 1}], [expr {($i % 3000) + 1}], 0, 'BAR')"]
        pg_result $res -clear
        incr ostat_cnt
    }
}

set end_time [clock milliseconds]
set elapsed_ms [expr {$end_time - $start_time}]
set elapsed_sec [expr {$elapsed_ms / 1000.0}]
set tps [expr {$iterations / $elapsed_sec}]
set tpm [expr {$tps * 60.0}]
set nopm [expr {($neword_cnt / $elapsed_sec) * 60.0}]
set avg_latency [expr {$elapsed_ms / double($iterations)}]

puts "=================================================="
puts "  H2 DATABASE RUST TPROC-C BENCHMARK RESULT"
puts "=================================================="
puts "Total Transactions: $iterations"
puts "  - NEWORD:   $neword_cnt ([format "%.1f" [expr {100.0 * $neword_cnt / $iterations}]]%)"
puts "  - PAYMENT:  $payment_cnt ([format "%.1f" [expr {100.0 * $payment_cnt / $iterations}]]%)"
puts "  - SLEV:     $slev_cnt ([format "%.1f" [expr {100.0 * $slev_cnt / $iterations}]]%)"
puts "  - DELIVERY: $delivery_cnt ([format "%.1f" [expr {100.0 * $delivery_cnt / $iterations}]]%)"
puts "  - OSTAT:    $ostat_cnt ([format "%.1f" [expr {100.0 * $ostat_cnt / $iterations}]]%)"
puts "Elapsed Time:       [format "%.3f" $elapsed_sec] sec ($elapsed_ms ms)"
puts "Average Latency:    [format "%.3f" $avg_latency] ms / transaction"
puts "Throughput (TPS):   [format "%.2f" $tps] TPS"
puts "Throughput (TPM):   [format "%.0f" $tpm] TPM"
puts "HammerDB NOPM:      [format "%.0f" $nopm] NOPM"
puts "=================================================="

pg_disconnect $lda
exit
