#!/usr/bin/env python3
"""Parses PostgreSQL WAL records and correlates them with server and audit logs.

Extracts transaction ID (XID), commit timestamp, commit status, LSN, and
the executed SQL statement or physical WAL operations.

Example usage:
    # From waldump stdout:
    pg_waldump ... | ./scripts/lib/parse-transactions.py

    # With correlated server log file:
    pg_waldump ... | ./scripts/lib/parse-transactions.py --logs /path/to/server.log

    # Output JSON:
    pg_waldump ... | ./scripts/lib/parse-transactions.py --json
"""

import argparse
import datetime
import json
import re
import sys
from typing import Any, Dict, List, Optional, Tuple


# Regex patterns for WAL records produced by pg_waldump
RE_WAL_RECORD = re.compile(
    r"rmgr:\s+(?P<rmgr>\S+)\s+len\s*\([^)]+\):\s+\d+/\s*\d+,\s+"
    r"tx:\s+(?P<tx>\d+),\s+lsn:\s+(?P<lsn>[0-9A-Fa-f]+/[0-9A-Fa-f]+),\s+"
    r"prev\s+[0-9A-Fa-f]+/[0-9A-Fa-f]+,\s+desc:\s+(?P<desc>.*)"
)

RE_COMMIT_DESC = re.compile(
    r"^(?P<action>COMMIT|ABORT)\s+(?P<timestamp>\d{4}-\d{2}-\d{2}\s+\d{2}:\d{2}:\d{2}(?:\.\d+)?)\s+UTC"
)

# Regex pattern for PostgreSQL server log statement lines
# Handles both standard Postgres prefix and sidecar container log format:
# "2026-11-12T06:04:45.094754Z  WARN postgres: 2026-11-12 06:04:45.094 GMT [763] tx=0: LOG:  statement: ..."
# "2026-11-12 06:04:45.094 GMT [763] tx=0: LOG:  statement: ..."
RE_LOG_STATEMENT = re.compile(
    r"(?P<ts>\d{4}-\d{2}-\d{2}[ T]\d{2}:\d{2}:\d{2}(?:\.\d+)?)(?:\s*(?:GMT|UTC|Z|[+-]\d{2}:?\d{2}))?"
    r".*?\[(?P<pid>\d+)\].*?(?:tx=(?P<tx>[0-9a-fA-F]+):\s*)?LOG:\s+statement:\s*(?P<stmt>.*)"
)

# Regex matching SQL continuation lines (indented with tab or spaces, optionally prefixed by container log wrapper)
RE_CONTINUATION = re.compile(
    r"^(?:.*?WARN postgres:\s+)?(?:\t|\s{4,})(?P<content>.*)$"
)


def parse_timestamp_epoch(ts_str: str) -> Optional[float]:
    """Converts a timestamp string into UTC epoch seconds.

    Handles ISO-8601, Postgres standard, and common log timestamps.

    Args:
        ts_str: String containing date and time.

    Returns:
        Float epoch timestamp in seconds, or None if parsing fails.

    Example:
        >>> ts = parse_timestamp_epoch("2026-11-12 06:04:45.094911")
        >>> isinstance(ts, float)
        True
    """
    clean_ts = ts_str.strip()
    for suffix in (" UTC", " GMT", "Z"):
        if clean_ts.endswith(suffix):
            clean_ts = clean_ts[: -len(suffix)].strip()
            break

    # Strip timezone offset if present (+00, +07:00, etc.)
    clean_ts = re.sub(r"[+-]\d{2}:?\d{2}$", "", clean_ts).strip()

    # Normalize ISO-8601 'T' separator between date and time to space
    if "T" in clean_ts:
        clean_ts = re.sub(r"(\d{4}-\d{2}-\d{2})T(\d{2}:)", r"\1 \2", clean_ts)

    # Truncate sub-second digits beyond 6 (microseconds) for Python strptime
    clean_ts = re.sub(r"(\.\d{6})\d+", r"\1", clean_ts)

    formats = [
        "%Y-%m-%d %H:%M:%S.%f",
        "%Y-%m-%d %H:%M:%S",
    ]
    for fmt in formats:
        try:
            dt = datetime.datetime.strptime(clean_ts, fmt)
            return dt.replace(tzinfo=datetime.timezone.utc).timestamp()
        except ValueError:
            continue
    return None


def parse_wal_lines(lines: List[str]) -> Dict[int, Dict[str, Any]]:
    """Parses raw lines from pg_waldump into structured transaction records.

    Args:
        lines: Iterator or list of raw text lines from pg_waldump.

    Returns:
        Dictionary mapping transaction XID (int) to transaction details:
        {
            "xid": 753,
            "status": "COMMIT",
            "timestamp": "2026-11-12 06:04:45.094911 UTC",
            "epoch": 1794463485.094911,
            "lsn": "0/04020D80",
            "operations": ["Heap INSERT (rel 1663/5/16384)"],
            "statement": None
        }

    Example:
        >>> data = ["rmgr: Transaction len: 34, tx: 753, lsn: 0/1, prev 0/0, desc: COMMIT 2026-11-12 00:00:00 UTC"]
        >>> res = parse_wal_lines(data)
        >>> 753 in res
        True
    """
    transactions: Dict[int, Dict[str, Any]] = {}

    for line in lines:
        match = RE_WAL_RECORD.search(line)
        if not match:
            continue

        rmgr = match.group("rmgr")
        tx_str = match.group("tx")
        lsn = match.group("lsn")
        desc = match.group("desc")

        try:
            xid = int(tx_str)
        except ValueError:
            continue

        if xid == 0:
            # Skip non-transactional system operations unless explicitly tracked
            continue

        if xid not in transactions:
            transactions[xid] = {
                "xid": xid,
                "status": "IN_PROGRESS",
                "timestamp": None,
                "epoch": None,
                "lsn": lsn,
                "operations": [],
                "statement": None,
            }

        tx_entry = transactions[xid]
        tx_entry["lsn"] = lsn

        if rmgr == "Transaction":
            commit_match = RE_COMMIT_DESC.search(desc)
            if commit_match:
                action = commit_match.group("action")
                ts_str = commit_match.group("timestamp") + " UTC"
                tx_entry["status"] = action
                tx_entry["timestamp"] = ts_str
                tx_entry["epoch"] = parse_timestamp_epoch(ts_str)
        elif rmgr in ("Heap", "Heap2"):
            # Extract table operation and relation OID
            op = "MOD"
            if "INSERT" in desc:
                op = "INSERT"
            elif "UPDATE" in desc:
                op = "UPDATE"
            elif "DELETE" in desc:
                op = "DELETE"

            rel_match = re.search(r"rel\s+([0-9/]+)", desc)
            rel_info = f" rel {rel_match.group(1)}" if rel_match else ""
            summary = f"Heap {op}{rel_info}"
            if summary not in tx_entry["operations"] and len(tx_entry["operations"]) < 5:
                tx_entry["operations"].append(summary)
        elif rmgr == "Storage":
            if "CREATE" in desc:
                tx_entry["operations"].append("Storage CREATE (DDL)")
        elif rmgr == "Btree":
            if "INSERT" in desc and "Btree INSERT" not in tx_entry["operations"]:
                tx_entry["operations"].append("Btree INSERT")

    return transactions


def parse_log_statements(log_lines: List[str]) -> List[Dict[str, Any]]:
    """Extracts executed SQL statements and their timestamps from server logs.

    Args:
        log_lines: Lines from PostgreSQL server log or docker logs.

    Returns:
        List of dictionaries with parsed statement metadata:
        [{"timestamp": str, "epoch": float, "pid": int, "tx": int, "statement": str}]

    Example:
        >>> logs = ["2026-11-12 06:04:45.094 GMT [763] tx=0: LOG:  statement: SELECT 1;"]
        >>> stmts = parse_log_statements(logs)
        >>> len(stmts) == 1
        True
    """
    statements: List[Dict[str, Any]] = []
    current_entry: Optional[Dict[str, Any]] = None

    def flush_entry() -> None:
        nonlocal current_entry
        if current_entry:
            # Join multiple continuation lines into one clean statement string
            combined_text = " ".join(" ".join(current_entry["lines"]).split())
            if combined_text:
                statements.append({
                    "timestamp": current_entry["timestamp"],
                    "epoch": current_entry["epoch"],
                    "pid": current_entry["pid"],
                    "tx": current_entry["tx"],
                    "statement": combined_text,
                })
            current_entry = None

    for line in log_lines:
        match = RE_LOG_STATEMENT.search(line)
        if match:
            flush_entry()
            raw_ts = match.group("ts")
            epoch = parse_timestamp_epoch(raw_ts)
            pid = int(match.group("pid"))
            tx_str = match.group("tx")
            tx = int(tx_str, 16) if tx_str else 0
            initial_stmt = match.group("stmt").strip()

            current_entry = {
                "timestamp": raw_ts,
                "epoch": epoch,
                "pid": pid,
                "tx": tx,
                "lines": [initial_stmt] if initial_stmt else [],
            }
            continue

        if current_entry:
            cont_match = RE_CONTINUATION.match(line)
            if cont_match:
                content = cont_match.group("content").strip()
                if content:
                    current_entry["lines"].append(content)
            elif line.startswith("\t") or line.startswith("    "):
                stripped = line.strip()
                if stripped:
                    current_entry["lines"].append(stripped)
            else:
                flush_entry()

    flush_entry()
    return statements


def parse_audit_entries(audit_data: str) -> List[Dict[str, Any]]:
    """Extracts SQL statements from audit batch JSON contents.

    Args:
        audit_data: JSON string containing audit events array.

    Returns:
        List of statements extracted from audit events.
    """
    entries: List[Dict[str, Any]] = []
    try:
        data = json.loads(audit_data)
        if not isinstance(data, list):
            data = [data]
        for item in data:
            if not isinstance(item, dict):
                continue
            occurred_at = item.get("occurred_at")
            detail = item.get("detail", "")
            epoch = parse_timestamp_epoch(occurred_at) if occurred_at else None

            # Look for SQL statement in detail
            sql_text = None
            if "Dangerous SQL executed" in detail:
                parts = detail.split(":", 1)
                if len(parts) > 1:
                    sql_text = parts[1].strip()
            elif item.get("kind") == "dangerous_sql":
                sql_text = detail

            if sql_text and epoch is not None:
                # Clean up newlines for display
                sql_one_line = " ".join(sql_text.split())
                entries.append({
                    "timestamp": occurred_at,
                    "epoch": epoch,
                    "pid": 0,
                    "tx": 0,
                    "statement": f"[AUDIT] {sql_one_line}",
                })
    except Exception:
        pass
    return entries


READONLY_SQL_PREFIXES = ("SELECT", "SHOW", "SET", "EXPLAIN")


def correlate_statements_with_wal(
    transactions: Dict[int, Dict[str, Any]],
    statements: List[Dict[str, Any]],
    max_time_delta_sec: float = 5.0,
) -> None:
    """Correlates SQL statements with WAL transactions by XID and timestamp.

    Write and DDL statements are prioritized; read-only statements (like SELECT)
    are skipped for mutating WAL transactions.

    Args:
        transactions: Transactions dictionary updated in-place.
        statements: Parsed SQL statements from server/audit logs.
        max_time_delta_sec: Maximum time window between statement and commit.

    Example:
        >>> txs = {753: {"xid": 753, "epoch": 100.0, "statement": None}}
        >>> stmts = [{"tx": 753, "epoch": 99.9, "statement": "INSERT 1"}]
        >>> correlate_statements_with_wal(txs, stmts)
        >>> txs[753]["statement"]
        'INSERT 1'
    """
    # Prefer write/modifying/DDL statements for WAL transaction correlation
    write_statements = [
        s for s in statements
        if not s["statement"].strip().upper().startswith(READONLY_SQL_PREFIXES)
    ]
    candidate_statements = write_statements if write_statements else statements

    sorted_txs = sorted(
        [tx for tx in transactions.values() if tx.get("epoch") is not None],
        key=lambda x: x["epoch"],
    )

    for tx in sorted_txs:
        xid = tx["xid"]
        tx_epoch = tx["epoch"]

        # 1. Try exact XID match first
        exact_match = next(
            (s["statement"] for s in candidate_statements if s.get("tx") == xid),
            None,
        )
        if exact_match:
            tx["statement"] = exact_match
            continue

        # 2. Try timestamp proximity match (statement occurred right before or at commit)
        candidate: Optional[Tuple[float, str]] = None
        for s in candidate_statements:
            s_epoch = s.get("epoch")
            if s_epoch is None:
                continue

            # Statement must precede or roughly match commit timestamp
            delta = tx_epoch - s_epoch
            if -0.5 <= delta <= max_time_delta_sec:
                abs_delta = abs(delta)
                if candidate is None or abs_delta < candidate[0]:
                    candidate = (abs_delta, s["statement"])

        if candidate:
            tx["statement"] = candidate[1]


def format_table(
    records: List[Dict[str, Any]],
    show_wal_ops: bool = False,
    max_stmt_len: int = 100,
) -> str:
    """Formats transaction records into an aligned plain text table.

    Args:
        records: List of transaction dictionaries.
        show_wal_ops: Whether physical WAL operations are included.
        max_stmt_len: Maximum width for the statement column (0 for no truncation).

    Returns:
        Formatted ASCII table string without ANSI escape sequences.
    """
    if not records:
        return "No transactions found."

    stmt_header = "STATEMENT / WAL OPERATIONS" if show_wal_ops else "EXECUTED STATEMENT"
    headers = ["XID", "COMMIT TIMESTAMP (UTC)", "STATUS", "LSN", stmt_header]
    rows: List[List[str]] = []

    for r in records:
        xid_str = str(r["xid"])
        ts_str = r.get("timestamp") or "N/A"
        status_str = r.get("status") or "UNKNOWN"
        lsn_str = r.get("lsn") or "N/A"

        # Determine description: SQL statement takes priority, else WAL operations
        stmt = r.get("statement")
        if stmt:
            desc_str = stmt
        elif r.get("operations"):
            desc_str = ", ".join(r["operations"])
        else:
            desc_str = "(no row modification records)"

        # Truncate overly long statements for table display if max_stmt_len > 0
        if max_stmt_len > 0 and len(desc_str) > max_stmt_len:
            desc_str = desc_str[: max_stmt_len - 3] + "..."

        rows.append([xid_str, ts_str, status_str, lsn_str, desc_str])

    # Compute column widths
    col_widths = [len(h) for h in headers]
    for row in rows:
        for i, val in enumerate(row):
            if len(val) > col_widths[i]:
                col_widths[i] = len(val)

    # Build output
    header_line = " | ".join(h.ljust(col_widths[i]) for i, h in enumerate(headers))
    separator_line = "-+-".join("-" * col_widths[i] for i in range(len(headers)))

    result_lines = [header_line, separator_line]
    for row in rows:
        line = " | ".join(val.ljust(col_widths[i]) for i, val in enumerate(row))
        result_lines.append(line)

    return "\n".join(result_lines)


def main() -> None:
    """Command-line entry point."""
    parser = argparse.ArgumentParser(
        description="Extract and correlate transaction IDs, commit timestamps, and executed statements from WAL and logs."
    )
    parser.add_argument(
        "--waldump",
        type=str,
        default=None,
        help="Path to file containing raw pg_waldump output (default: read from stdin).",
    )
    parser.add_argument(
        "--logs",
        type=str,
        default=None,
        help="Path to PostgreSQL server log file to correlate executed statements.",
    )
    parser.add_argument(
        "--audit",
        type=str,
        default=None,
        help="Path to audit batch JSON file to correlate high-level cluster statements.",
    )
    parser.add_argument(
        "--status",
        choices=["all", "commit", "abort"],
        default="all",
        help="Filter transactions by commit status (default: all).",
    )
    parser.add_argument(
        "--wal-ops",
        "--all",
        action="store_true",
        dest="show_wal_ops",
        help="Include internal transactions with only raw physical WAL operations (Heap INSERT/MOD/etc.) and no executed SQL statements.",
    )
    parser.add_argument(
        "--max-width",
        type=int,
        default=100,
        help="Maximum column width for executed statements in plain-text table (default: 100, 0 for unlimited).",
    )
    parser.add_argument(
        "--limit",
        type=int,
        default=None,
        help="Maximum number of transactions to display.",
    )
    parser.add_argument(
        "--json",
        action="store_true",
        help="Output results in JSON format instead of plain text table.",
    )

    args = parser.parse_args()

    # Read WAL dump input
    if args.waldump:
        with open(args.waldump, "r", encoding="utf-8", errors="replace") as f:
            wal_lines = f.readlines()
    else:
        wal_lines = sys.stdin.readlines()

    transactions = parse_wal_lines(wal_lines)

    # Collect statement logs
    statements: List[Dict[str, Any]] = []

    if args.logs:
        try:
            with open(args.logs, "r", encoding="utf-8", errors="replace") as f:
                log_lines = f.readlines()
            statements.extend(parse_log_statements(log_lines))
        except Exception as e:
            sys.stderr.write(f"Warning: Failed to read log file {args.logs}: {e}\n")

    if args.audit:
        try:
            with open(args.audit, "r", encoding="utf-8", errors="replace") as f:
                audit_content = f.read()
            statements.extend(parse_audit_entries(audit_content))
        except Exception as e:
            sys.stderr.write(f"Warning: Failed to read audit file {args.audit}: {e}\n")

    if statements:
        correlate_statements_with_wal(transactions, statements)

    # Sort transactions by commit timestamp / XID
    record_list = list(transactions.values())
    record_list.sort(key=lambda r: (r.get("epoch") or 0.0, r["xid"]))

    # Filter out internal transactions with only raw physical WAL operations unless explicitly requested
    has_statements = any(r.get("statement") for r in record_list)
    if not args.show_wal_ops and has_statements:
        record_list = [r for r in record_list if r.get("statement") is not None]

    # Filter by status if specified
    if args.status == "commit":
        record_list = [r for r in record_list if r.get("status") == "COMMIT"]
    elif args.status == "abort":
        record_list = [r for r in record_list if r.get("status") == "ABORT"]

    # Apply limit
    if args.limit and args.limit > 0:
        record_list = record_list[-args.limit :]

    if args.json:
        print(json.dumps(record_list, indent=2))
    else:
        print(format_table(record_list, show_wal_ops=args.show_wal_ops, max_stmt_len=args.max_width))


if __name__ == "__main__":
    main()
