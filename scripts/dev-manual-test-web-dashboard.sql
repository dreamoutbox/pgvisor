DROP TABLE IF EXISTS pgvisor_demo;

CREATE TABLE pgvisor_demo (
    id SERIAL PRIMARY KEY,
    name VARCHAR(100) NOT NULL,
    status VARCHAR(50) DEFAULT 'active',
    counter INT DEFAULT 0,
    created_at TIMESTAMPTZ DEFAULT NOW(),
    updated_at TIMESTAMPTZ DEFAULT NOW()
);

INSERT INTO pgvisor_demo (name, status, counter) VALUES
    ('alpha', 'active', 10),
    ('beta', 'active', 20),
    ('gamma', 'pending', 30),
    ('delta', 'archived', 40);

DROP TABLE pgvisor_demo;

SELECT id, name, status, counter, created_at FROM pgvisor_demo ORDER BY id;

INSERT INTO pgvisor_demo (name, status, counter) VALUES
    ('echo', 'active', 50);

INSERT INTO pgvisor_demo (name, status, counter) VALUES
    ('foxtrot', 'pending', 60);

INSERT INTO pgvisor_demo (name, status, counter) VALUES
    ('golf', 'archived', 70);
