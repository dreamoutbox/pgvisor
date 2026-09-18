from dataclasses import dataclass
from datetime import datetime
from typing import List, Optional

from .database import execute_query, execute_write


@dataclass
class DemoItem:
    """Represents a record in the demo_items table."""

    id: Optional[int]
    title: str
    category: str
    description: str
    status: str
    created_at: Optional[datetime] = None
    updated_at: Optional[datetime] = None

    @classmethod
    def from_row(cls, row: dict) -> "DemoItem":
        """Construct a DemoItem from a database dictionary row."""
        return cls(
            id=row.get("id"),
            title=row.get("title", ""),
            category=row.get("category", "General"),
            description=row.get("description", ""),
            status=row.get("status", "Active"),
            created_at=row.get("created_at"),
            updated_at=row.get("updated_at"),
        )

    @classmethod
    def all(cls, status_filter: Optional[str] = None, search: Optional[str] = None) -> List["DemoItem"]:
        """Retrieve all items, optionally filtered by status or search query."""
        sql = "SELECT id, title, category, description, status, created_at, updated_at FROM demo_items WHERE 1=1"
        params = []

        if status_filter and status_filter.lower() != "all":
            sql += " AND status = %s"
            params.append(status_filter)

        if search:
            sql += " AND (title ILIKE %s OR description ILIKE %s OR category ILIKE %s)"
            pattern = f"%{search}%"
            params.extend([pattern, pattern, pattern])

        sql += " ORDER BY id DESC"
        rows = execute_query(sql, tuple(params) if params else None)
        return [cls.from_row(row) for row in rows]

    @classmethod
    def find(cls, item_id: int) -> Optional["DemoItem"]:
        """Find a single item by primary key."""
        sql = "SELECT id, title, category, description, status, created_at, updated_at FROM demo_items WHERE id = %s"
        rows = execute_query(sql, (item_id,))
        if rows:
            return cls.from_row(rows[0])
        return None

    @classmethod
    def create(cls, title: str, category: str, description: str, status: str) -> "DemoItem":
        """Insert a new demo item and return the created entity."""
        sql = """
        INSERT INTO demo_items (title, category, description, status, created_at, updated_at)
        VALUES (%s, %s, %s, %s, NOW(), NOW())
        RETURNING id, title, category, description, status, created_at, updated_at;
        """
        row = execute_write(sql, (title, category, description, status), returning=True)
        return cls.from_row(row)

    @classmethod
    def update(cls, item_id: int, title: str, category: str, description: str, status: str) -> bool:
        """Update an existing demo item."""
        sql = """
        UPDATE demo_items
        SET title = %s, category = %s, description = %s, status = %s, updated_at = NOW()
        WHERE id = %s;
        """
        affected = execute_write(sql, (title, category, description, status, item_id))
        return affected > 0

    @classmethod
    def delete(cls, item_id: int) -> bool:
        """Delete an item by primary key."""
        sql = "DELETE FROM demo_items WHERE id = %s;"
        affected = execute_write(sql, (item_id,))
        return affected > 0

    @classmethod
    def count_by_status(cls) -> dict:
        """Return counts grouped by status."""
        sql = "SELECT status, COUNT(*) AS count FROM demo_items GROUP BY status;"
        rows = execute_query(sql)
        counts = {"Active": 0, "Pending": 0, "Archived": 0, "Total": 0}
        total = 0
        for r in rows:
            st = r["status"]
            cnt = r["count"]
            counts[st] = cnt
            total += cnt
        counts["Total"] = total
        return counts

    @classmethod
    def seed_defaults(cls) -> int:
        """Seed sample demonstration records."""
        samples = [
            ("Verify L7 Read/Write Splitting", "Proxy", "Validate that read queries target standby and writes target leader.", "Active"),
            ("Test Raft Lease Fencing", "High Availability", "Simulate leader isolation and immediate fencing via pg_ctl stop -m immediate.", "Pending"),
            ("Continuous WAL Archival", "Storage", "Verify OpenDAL continuous archive_command to MinIO / S3.", "Active"),
            ("Cluster Resync & Timeline Realign", "Replication", "Test standby re-cloning following leader timeline rewind.", "Archived"),
        ]
        count = 0
        for title, cat, desc, st in samples:
            cls.create(title, cat, desc, st)
            count += 1
        return count
