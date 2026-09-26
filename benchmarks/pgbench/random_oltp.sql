\set aid1 random(1, 1000000)
\set aid2 random(1, 1000000)
\set aid3 random(1, 1000000)
\set aid4 random(1, 1000000)
\set aid5 random(1, 1000000)
\set delta random(-5000, 5000)
SELECT abalance FROM pgbench_accounts WHERE aid = :aid1;
SELECT abalance FROM pgbench_accounts WHERE aid = :aid2;
SELECT abalance FROM pgbench_accounts WHERE aid = :aid3;
SELECT abalance FROM pgbench_accounts WHERE aid = :aid4;
UPDATE pgbench_accounts SET abalance = abalance + :delta WHERE aid = :aid5;
