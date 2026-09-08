"""Read-only, value-free cc-switch database structure diagnostic.

The caller must provide the database path explicitly. Output contains only
allowlisted schema presence and row counts; provider names, endpoints,
credentials, settings values, and the local path are never emitted.
"""

from __future__ import annotations

import argparse
import json
import sqlite3
from pathlib import Path
from urllib.parse import quote


KNOWN_TABLES = (
    "providers",
    "proxy_config",
    "settings",
)


def inspect_database(database_path: Path) -> dict[str, object]:
    if database_path.is_symlink():
        raise ValueError("database_unavailable")
    resolved = database_path.resolve(strict=True)
    if not resolved.is_file():
        raise ValueError("database_unavailable")
    uri_path = quote(resolved.as_posix(), safe="/:")
    connection = sqlite3.connect(f"file:{uri_path}?mode=ro", uri=True)
    try:
        connection.execute("PRAGMA query_only = ON")
        present = {
            row[0]
            for row in connection.execute(
                "SELECT name FROM sqlite_master WHERE type = 'table'"
            )
            if isinstance(row[0], str)
        }
        row_counts: dict[str, int] = {}
        for table in KNOWN_TABLES:
            if table in present:
                quoted_table = '"' + table.replace('"', '""') + '"'
                row_counts[table] = int(
                    connection.execute(
                        f"SELECT COUNT(*) FROM {quoted_table}"
                    ).fetchone()[0]
                )
        return {
            "schema_version": 1,
            "known_tables_present": [
                table for table in KNOWN_TABLES if table in present
            ],
            "unknown_table_count": len(present.difference(KNOWN_TABLES)),
            "row_counts": row_counts,
        }
    finally:
        connection.close()


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Inspect cc-switch SQLite structure without printing values."
    )
    parser.add_argument("--database", required=True, type=Path)
    arguments = parser.parse_args()
    try:
        report = inspect_database(arguments.database)
    except (OSError, sqlite3.Error, ValueError):
        print(json.dumps({"schema_version": 1, "error": "database_unavailable"}))
        return 1
    print(json.dumps(report, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
