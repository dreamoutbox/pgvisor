from typing import Any, Dict, Optional

from .database import execute_query, execute_write


class ClusterModel:
    """Queries cluster and backend node metadata via PgVisor proxy."""

    @classmethod
    def get_cluster_info(cls) -> Dict[str, Any]:
        """Fetch PostgreSQL connection and server status."""
        try:
            sql = """
            SELECT
                version() AS pg_version,
                current_database() AS db_name,
                current_user AS db_user,
                inet_server_addr() AS server_addr,
                inet_server_port() AS server_port,
                pg_is_in_recovery() AS in_recovery;
            """
            rows = execute_query(sql)
            if rows:
                row = rows[0]
                return {
                    "connected": True,
                    "pg_version": row.get("pg_version", ""),
                    "db_name": row.get("db_name", ""),
                    "db_user": row.get("db_user", ""),
                    "server_addr": str(row.get("server_addr") or "unknown"),
                    "server_port": row.get("server_port", 5432),
                    "in_recovery": row.get("in_recovery", False),
                    "node_role": "Standby (Replica)" if row.get("in_recovery") else "Leader (Primary)",
                    "error": None,
                }
        except Exception as e:
            return {
                "connected": False,
                "pg_version": None,
                "db_name": None,
                "db_user": None,
                "server_addr": None,
                "server_port": None,
                "in_recovery": None,
                "node_role": "Unavailable",
                "error": str(e),
            }
        return {"connected": False, "error": "No result returned"}

    @classmethod
    def test_routing(cls) -> Dict[str, Any]:
        """Execute a read query and a write query to demonstrate L7 read/write splitting."""
        results = {
            "read_test": None,
            "write_test": None,
            "error": None,
        }
        try:
            # 1. Pure Read Query (routes to standby replica if available)
            read_sql = """
            SELECT
                pg_is_in_recovery() AS in_recovery,
                inet_server_addr() AS server_addr,
                inet_server_port() AS server_port,
                NOW() AS query_time;
            """
            read_res = execute_query(read_sql)
            if read_res:
                r = read_res[0]
                results["read_test"] = {
                    "server_addr": str(r.get("server_addr") or "internal"),
                    "server_port": r.get("server_port", 5432),
                    "in_recovery": r.get("in_recovery", False),
                    "routed_role": "Standby Replica" if r.get("in_recovery") else "Leader Primary",
                    "query_time": str(r.get("query_time")),
                }

            # 2. Write Query (routes to Raft leader primary)
            write_sql = """
            UPDATE demo_items
            SET updated_at = NOW()
            WHERE id = (SELECT id FROM demo_items ORDER BY id ASC LIMIT 1)
            RETURNING pg_is_in_recovery() AS in_recovery, inet_server_addr() AS server_addr, inet_server_port() AS server_port;
            """
            write_res = execute_write(write_sql, returning=True)
            if write_res:
                results["write_test"] = {
                    "server_addr": str(write_res.get("server_addr") or "internal"),
                    "server_port": write_res.get("server_port", 5432),
                    "in_recovery": write_res.get("in_recovery", False),
                    "routed_role": "Leader Primary" if not write_res.get("in_recovery") else "Standby Replica",
                }
            else:
                results["write_test"] = {
                    "server_addr": "internal",
                    "server_port": 5432,
                    "in_recovery": False,
                    "routed_role": "Leader Primary (0 rows updated)",
                }

        except Exception as e:
            results["error"] = str(e)

        return results
