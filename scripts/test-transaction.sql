-- PgVisor Transaction Test Script
-- Exercises BEGIN/COMMIT, BEGIN/ROLLBACK, and error-mid-transaction recovery
-- through the proxy to verify correct backend connection pinning and isolation.
\echo '---------------------------------------------------------'
\echo '  PgVisor Transaction Test'
\echo '---------------------------------------------------------'

-- Setup
\echo ''
\echo '[setup] Creating test table txn_test...'
DROP TABLE IF EXISTS txn_test;
CREATE TABLE txn_test (id SERIAL PRIMARY KEY, val TEXT NOT NULL);

-- ---------------------------------------------------------------
-- Scenario 1: BEGIN ... COMMIT
-- Inserts inside the block must be visible after COMMIT.
-- ---------------------------------------------------------------
\echo ''
\echo '[1/3] Scenario: BEGIN...COMMIT'
BEGIN;
  INSERT INTO txn_test (val) VALUES ('alpha');
  INSERT INTO txn_test (val) VALUES ('beta');
  -- Read inside the transaction: connection is pinned to leader so
  -- we see our own writes immediately.
  SELECT count(*) AS rows_in_tx FROM txn_test;
COMMIT;

-- Verify committed rows (shell script will poll-assert count = 2)
SELECT count(*) AS committed_rows FROM txn_test;

-- ---------------------------------------------------------------
-- Scenario 2: BEGIN ... ROLLBACK
-- Rolled-back inserts must NOT be visible after ROLLBACK.
-- ---------------------------------------------------------------
\echo ''
\echo '[2/3] Scenario: BEGIN...ROLLBACK'
BEGIN;
  INSERT INTO txn_test (val) VALUES ('should-vanish');
ROLLBACK;

-- Verify rolled-back row is absent (shell script will poll-assert count = 0)
SELECT count(*) AS rolled_back_rows FROM txn_test WHERE val = 'should-vanish';

-- ---------------------------------------------------------------
-- Scenario 3: Error mid-transaction + ROLLBACK
-- An error inside a transaction must leave no partial writes.
-- ---------------------------------------------------------------
\echo ''
\echo '[3/3] Scenario: error mid-transaction + ROLLBACK'
BEGIN;
  INSERT INTO txn_test (val) VALUES ('pre-error');
  SELECT 1/0;
ROLLBACK;

-- Verify the pre-error row was discarded (shell script will poll-assert count = 0)
SELECT count(*) AS error_rows FROM txn_test WHERE val = 'pre-error';

-- Cleanup
\echo ''
\echo '[cleanup] Dropping txn_test...'
DROP TABLE txn_test;

\echo '---------------------------------------------------------'
\echo '  Transaction SQL fixture completed.'
\echo '---------------------------------------------------------'
