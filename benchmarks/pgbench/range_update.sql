\set start random(1, 999900)
UPDATE pgbench_accounts SET abalance = abalance + 1 WHERE aid BETWEEN :start AND :start + 10;
