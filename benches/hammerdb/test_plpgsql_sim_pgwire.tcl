#!/bin/tclsh
puts "=================================================="
puts "  Testing PL/pgSQL Simulation Layer over PGWire"
puts "=================================================="

package require Pgtcl

set lda [pg_connect -conninfo "host=host.docker.internal port=5433 dbname=mydb user=postgres"]
puts "Connected to h2database-rust: $lda"

# 1. Create table
puts "1. Creating table..."
set res [pg_exec $lda "CREATE TABLE IF NOT EXISTS item (i_id INT PRIMARY KEY, i_im_id INT, i_name VARCHAR(24), i_price DECIMAL(5,2), i_data VARCHAR(50))"]
puts "   CREATE TABLE status: [pg_result $res -status]"
pg_result $res -clear

# 2. Register PL/pgSQL function
puts "2. Registering PL/pgSQL function DBMS_RANDOM..."
set ddl_func "CREATE OR REPLACE FUNCTION DBMS_RANDOM (INTEGER, INTEGER) RETURNS INTEGER AS \$\$
DECLARE
start_int ALIAS FOR \$1;
end_int ALIAS FOR \$2;
BEGIN
RETURN trunc(random() * (end_int-start_int + 1) + start_int);
END;
\$\$ LANGUAGE 'plpgsql' STRICT;"
set res [pg_exec $lda $ddl_func]
puts "   CREATE FUNCTION status: [pg_result $res -status]"
pg_result $res -clear

# 3. Register PL/pgSQL procedure NEWORD
puts "3. Registering PL/pgSQL procedure NEWORD..."
set ddl_proc "CREATE OR REPLACE PROCEDURE NEWORD (
no_w_id IN INTEGER,
no_max_w_id IN INTEGER,
no_d_id IN INTEGER,
no_c_id IN INTEGER,
no_o_ol_cnt IN INTEGER,
no_c_discount INOUT NUMERIC,
no_c_last INOUT VARCHAR,
no_c_credit INOUT VARCHAR,
no_d_tax INOUT NUMERIC,
no_w_tax INOUT NUMERIC,
no_d_next_o_id INOUT INTEGER,
tstamp IN TIMESTAMP )
AS \$\$ BEGIN END; \$\$ LANGUAGE 'plpgsql';"
set res [pg_exec $lda $ddl_proc]
puts "   CREATE PROCEDURE status: [pg_result $res -status]"
pg_result $res -clear

# 4. Query pg_proc
puts "4. Querying pg_proc catalog..."
set res [pg_exec $lda "SELECT p.prokind, count(*) AS cnt FROM pg_proc p"]
puts "   SELECT pg_proc status: [pg_result $res -status]"
puts "   SELECT pg_proc result: [pg_result $res -list]"
pg_result $res -clear

# 5. Query DBMS_RANDOM
puts "5. Calling SELECT DBMS_RANDOM(1, 100)..."
set res [pg_exec $lda "SELECT DBMS_RANDOM(1, 100)"]
puts "   SELECT DBMS_RANDOM status: [pg_result $res -status]"
puts "   SELECT DBMS_RANDOM result: [pg_result $res -list]"
pg_result $res -clear

# 6. Call NEWORD procedure
puts "6. Calling CALL NEWORD(1, 1, 1, 100, 10, ...)..."
set res [pg_exec $lda "CALL neword(1, 1, 1, 100, 10, 0.0, '', '', 0.0, 0.0, 0, CURRENT_TIMESTAMP)"]
puts "   CALL NEWORD status: [pg_result $res -status]"
puts "   CALL NEWORD result: [pg_result $res -list]"
pg_result $res -clear

# 7. Call PAYMENT procedure
puts "7. Calling CALL PAYMENT(1, 1, 1, 1, 100, 0, 50.0, 'SMITH')..."
set res [pg_exec $lda "CALL payment(1, 1, 1, 1, 100, 0, 50.0, 'SMITH')"]
puts "   CALL PAYMENT status: [pg_result $res -status]"
puts "   CALL PAYMENT result: [pg_result $res -list]"
pg_result $res -clear

pg_disconnect $lda
puts "=================================================="
puts "  PL/pgSQL Simulation Layer Test Completed"
puts "=================================================="
exit
