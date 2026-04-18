# /// script
# requires-python = ">=3.11"
# ///
"""Download airport data from OurAirports and generate data/airports.csv."""

import csv
import io
import urllib.request
from pathlib import Path

OURAIRPORTS_URL = "https://raw.githubusercontent.com/davidmegginson/ourairports-data/main/airports.csv"
OUTPUT_PATH = Path(__file__).resolve().parent.parent / "data" / "airports.csv"


def main():
    print(f"Downloading {OURAIRPORTS_URL}...")
    response = urllib.request.urlopen(OURAIRPORTS_URL)
    raw = response.read().decode("utf-8")

    reader = csv.DictReader(io.StringIO(raw))
    entries = []
    for row in reader:
        ident = row.get("ident", "").strip()
        iata = row.get("iata_code", "").strip()
        name = row.get("name", "").strip()
        lat = row.get("latitude_deg", "").strip()
        lon = row.get("longitude_deg", "").strip()

        # Skip rows without ident or coordinates
        if not ident or not lat or not lon:
            continue

        # Escape commas in airport names (CSV quoting handled by csv.writer)
        entries.append((ident, iata, name, lat, lon))

    # Sort by ident for deterministic output
    entries.sort(key=lambda e: e[0])

    # Write output (no header row, matching plz.csv convention)
    OUTPUT_PATH.parent.mkdir(parents=True, exist_ok=True)
    with open(OUTPUT_PATH, "w", newline="") as f:
        writer = csv.writer(f, lineterminator="\n")
        for entry in entries:
            writer.writerow(entry)

    iata_count = sum(1 for e in entries if e[1])
    print(f"Wrote {len(entries)} airports to {OUTPUT_PATH}")
    print(f"  {iata_count} with IATA codes")
    # Print samples
    for entry in entries[:3]:
        print(f"  {entry[0]} ({entry[1] or '-'}): {entry[2]}")
    print("  ...")
    for entry in entries[-3:]:
        print(f"  {entry[0]} ({entry[1] or '-'}): {entry[2]}")


if __name__ == "__main__":
    main()
