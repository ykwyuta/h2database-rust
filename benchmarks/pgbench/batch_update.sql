\set aid random(1, 1000000)
UPDATE pgbench_accounts SET abalance = abalance + 10, filler = 'batch_updated' WHERE aid = :aid;
