import logging
import time
from contextlib import contextmanager
from typing import Any, Dict, List, Optional, Tuple

import psycopg2
from psycopg2 import pool
from psycopg2.extras import RealDictCursor

from config import Config

logger = logging.getLogger(__name__)

# Module-level connection pool
_db_pool: Optional[pool.ThreadedConnectionPool] = None


def get_connection_pool(minconn: int = 1, maxconn: int = 10) -> pool.ThreadedConnectionPool:
    """Lazily initialize and return the threaded connection pool."""
    global _db_pool
    if _db_pool is None or _db_pool.closed:
        dsn = Config.get_dsn()
        logger.info("Initializing database connection pool to PgVisor proxy: host=%s port=%s", Config.DB_HOST, Config.DB_PORT)
        _db_pool = pool.ThreadedConnectionPool(minconn=minconn, maxconn=maxconn, dsn=dsn)
    return _db_pool


def close_connection_pool() -> None:
    """Close all connections in the pool."""
    global _db_pool
    if _db_pool and not _db_pool.closed:
        _db_pool.closeall()
        _db_pool = None
        logger.info("Database connection pool closed.")


@contextmanager
def get_db_connection(max_retries: int = 3, retry_delay: float = 0.5):
    """Context manager providing a pooled database connection with retry support."""
    last_err = None
    for attempt in range(1, max_retries + 1):
        conn = None
        try:
            pool_instance = get_connection_pool()
            conn = pool_instance.getconn()
            if conn.closed:
                pool_instance.putconn(conn, close=True)
                conn = pool_instance.getconn()
            yield conn
            return
        except (psycopg2.OperationalError, psycopg2.InterfaceError) as e:
            last_err = e
            logger.warning("Database connection attempt %d/%d failed: %s", attempt, max_retries, e)
            if conn:
                try:
                    pool_instance.putconn(conn, close=True)
                except Exception:
                    pass
                conn = None
            if attempt < max_retries:
                time.sleep(retry_delay * attempt)
        finally:
            if conn and not conn.closed:
                try:
                    get_connection_pool().putconn(conn)
                except Exception as e:
                    logger.error("Failed to return connection to pool: %s", e)

    raise psycopg2.OperationalError(f"Failed to connect to database after {max_retries} attempts: {last_err}")


def execute_query(sql: str, params: Optional[Tuple[Any, ...]] = None) -> List[Dict[str, Any]]:
    """Execute a read-only query and return list of dict rows."""
    with get_db_connection() as conn:
        with conn.cursor(cursor_factory=RealDictCursor) as cur:
            cur.execute(sql, params or ())
            return [dict(row) for row in cur.fetchall()]


def execute_write(
    sql: str,
    params: Optional[Tuple[Any, ...]] = None,
    returning: bool = False
) -> Any:
    """Execute a mutating write statement inside a committed transaction."""
    with get_db_connection() as conn:
        try:
            with conn.cursor(cursor_factory=RealDictCursor) as cur:
                cur.execute(sql, params or ())
                result = None
                if returning:
                    result = cur.fetchone()
                    if result:
                        result = dict(result)
                conn.commit()
                return result if returning else cur.rowcount
        except Exception:
            conn.rollback()
            raise


def init_db() -> None:
    """Create demo application tables if they do not already exist."""
    schema_sql = """
    CREATE TABLE IF NOT EXISTS demo_items (
        id SERIAL PRIMARY KEY,
        title VARCHAR(255) NOT NULL,
        category VARCHAR(50) DEFAULT 'General',
        description TEXT,
        status VARCHAR(20) DEFAULT 'Active',
        created_at TIMESTAMPTZ DEFAULT NOW(),
        updated_at TIMESTAMPTZ DEFAULT NOW()
    );
    """
    logger.info("Verifying and initializing demo_items table schema...")
    try:
        execute_write(schema_sql)
        # Check if table is empty, if so seed initial items
        count_rows = execute_query("SELECT COUNT(*) AS total FROM demo_items;")
        if count_rows and count_rows[0]["total"] == 0:
            seed_sql = """
            INSERT INTO demo_items (title, category, description, status) VALUES
                ('Evaluate PgVisor HA Proxy', 'Operations', 'Test read/write query routing and failover buffering.', 'Active'),
                ('Configure WAL Archiving', 'Backup', 'Verify continuous WAL upload to S3/MinIO bucket.', 'Active'),
                ('Review Raft Cluster Health', 'Consensus', 'Inspect OpenRaft leader lease and heartbeat status.', 'Pending'),
                ('Run Point-in-Time Recovery Test', 'Disaster Recovery', 'Validate snapshot restoration on standby node.', 'Archived');
            """
            execute_write(seed_sql)
            logger.info("Seeded initial demo items.")
    except Exception as e:
        logger.warning("Database init_db deferred or failed (cluster may still be starting): %s", e)
