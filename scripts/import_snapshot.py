#!/usr/bin/env python3
"""Import a real JSON snapshot produced by ../iphonechecker.py into Orchard."""

import argparse
import json
import sqlite3
from datetime import datetime
from pathlib import Path
from urllib.parse import urlparse


def timestamp(value):
    date = datetime.fromisoformat(value.replace("Z", "+00:00"))
    if date.tzinfo is None:
        raise ValueError("Snapshot timestamps must include a time zone.")
    return int(date.timestamp() * 1000)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("snapshot", type=Path)
    parser.add_argument("--database", required=True, type=Path, help="Database path shown in Orchard settings")
    args = parser.parse_args()
    if not args.database.is_file():
        parser.error("Open Orchard once to create its database first.")
    data = json.loads(args.snapshot.read_text())
    observed_at = timestamp(data["fetched_at"])
    source_at = timestamp(data["source_updated_at"]) if data.get("source_updated_at") else None
    parsed = urlparse(data["url"])
    identity = data["url"]
    if parsed.hostname == "iphonechecker.herokuapp.com":
        parts = parsed.path.strip("/").split("/")
        models = {"iDuo": "iphone-duo", "i18Pro": "iphone-18-pro", "i18ProMax": "iphone-18-pro-max"}
        if len(parts) != 4 or parts[0] != "q" or parts[2] not in models:
            parser.error("Unsupported legacy tracker address")
        identity = f"apple://us/{parts[1]}/{models[parts[2]]}/{parts[3]}"
    elif parsed.scheme != "apple" or parsed.hostname != "us":
        parser.error("Unsupported tracker identity")
    rows = data["inventory"]
    if not rows:
        parser.error("Snapshot has no inventory entries.")
    fields = ("store", "store_id", "city", "distance", "product", "part_number", "status", "pickup_quote")
    # Validate before starting a transaction. Imports retain the original collection time.
    values = [tuple(row[field] for field in fields) for row in rows]
    if not all(isinstance(value, str) for row in values for value in row):
        parser.error("Inventory fields must be strings.")
    with sqlite3.connect(args.database, timeout=30) as db:
        db.execute("PRAGMA foreign_keys=ON")
        db.execute("BEGIN IMMEDIATE")
        existing = db.execute("SELECT id FROM snapshots WHERE url=? AND observed_at=? AND error IS NULL",
                              (identity, observed_at)).fetchone()
        if existing:
            print(f"Snapshot already imported as observation #{existing[0]}.")
            return
        cursor = db.execute("INSERT INTO snapshots(observed_at,source_at,url,error) VALUES(?,?,?,NULL)",
                            (observed_at, source_at, identity))
        snapshot_id = cursor.lastrowid
        db.executemany("INSERT INTO inventory VALUES(?,?,?,?,?,?,?,?,?)",
                       [(snapshot_id, *row) for row in values])
    print(f"Imported observation #{snapshot_id}: {len(rows)} entries, collected {data['fetched_at']}.")


if __name__ == "__main__":
    main()
