-- PgVisor Demo Test Script: Table Creation, CRUD Operations, and Cleanup
\echo '---------------------------------------------------------'
\echo '  PgVisor PostgreSQL HA Cluster Demo: CRUD Test'
\echo '---------------------------------------------------------'

-- 1. Create Demo Table
\echo '\n[1/6] Creating test table pgvisor_demo...'
DROP TABLE IF EXISTS pgvisor_demo;
CREATE TABLE pgvisor_demo (
    id SERIAL PRIMARY KEY,
    name VARCHAR(100) NOT NULL,
    status VARCHAR(50) DEFAULT 'active',
    counter INT DEFAULT 0,
    created_at TIMESTAMPTZ DEFAULT NOW(),
    updated_at TIMESTAMPTZ DEFAULT NOW()
);

-- 2. CREATE (Insert Test Records)
\echo '\n[2/6] Inserting test rows (CREATE)...'
INSERT INTO pgvisor_demo (name, status, counter) VALUES
    ('alpha', 'active', 10),
    ('beta', 'active', 20),
    ('gamma', 'pending', 30),
    ('delta', 'archived', 40);

-- 3. READ (Query Initial State)
\echo '\n[3/6] Reading rows from cluster (READ)...'
SELECT id, name, status, counter, created_at FROM pgvisor_demo ORDER BY id;

-- 4. UPDATE (Modify Rows)
\echo '\n[4/6] Updating records (UPDATE)...'
UPDATE pgvisor_demo
SET status = 'active', counter = counter + 100, updated_at = NOW()
WHERE status = 'pending';

SELECT id, name, status, counter, updated_at FROM pgvisor_demo WHERE name = 'gamma';

-- 5. DELETE (Remove Specific Row)
\echo '\n[5/6] Deleting archived records (DELETE)...'
DELETE FROM pgvisor_demo WHERE status = 'archived';

SELECT id, name, status, counter FROM pgvisor_demo ORDER BY id;

-- 6. CLEANUP (Drop Table)
\echo '\n[6/6] Dropping demo table (CLEANUP)...'
DROP TABLE pgvisor_demo;

\echo '---------------------------------------------------------'
\echo '  CRUD Test Suite Completed Successfully on PgVisor!'
\echo '---------------------------------------------------------'
