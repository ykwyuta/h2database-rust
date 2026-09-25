SELECT abalance FROM pgbench_accounts WHERE aid = 100;
SELECT abalance FROM pgbench_accounts WHERE aid = 200;
SELECT abalance FROM pgbench_accounts WHERE aid = 300;
SELECT abalance FROM pgbench_accounts WHERE aid = 400;
UPDATE pgbench_accounts SET abalance = abalance + 1 WHERE aid = 100;
