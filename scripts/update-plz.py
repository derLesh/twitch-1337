# /// script
# requires-python = ">=3.11"
# ///
"""Download German postal code data from GeoNames and generate data/plz.csv."""

import csv
import io
import urllib.request
import zipfile
from collections import defaultdict
from pathlib import Path

GEONAMES_URL = "https://download.geonames.org/export/zip/DE.zip"
OUTPUT_PATH = Path(__file__).resolve().parent.parent / "data" / "plz.csv"


def main():
    print(f"Downloading {GEONAMES_URL}...")
    response = urllib.request.urlopen(GEONAMES_URL)
    zip_data = response.read()

    print("Extracting DE.txt...")
    with zipfile.ZipFile(io.BytesIO(zip_data)) as zf:
        raw = zf.read("DE.txt").decode("utf-8")

    # Group lat/lon by PLZ, then average
    coords: dict[str, list[tuple[float, float]]] = defaultdict(list)
    for line in raw.strip().splitlines():
        fields = line.split("\t")
        plz = fields[1]
        lat = float(fields[9])
        lon = float(fields[10])
        coords[plz].append((lat, lon))

    # Average coordinates per PLZ
    averaged: list[tuple[str, float, float]] = []
    for plz in sorted(coords):
        lats, lons = zip(*coords[plz])
        averaged.append((plz, sum(lats) / len(lats), sum(lons) / len(lons)))

    # Write output
    OUTPUT_PATH.parent.mkdir(parents=True, exist_ok=True)
    with open(OUTPUT_PATH, "w", newline="") as f:
        writer = csv.writer(f)
        for plz, lat, lon in averaged:
            writer.writerow([plz, f"{lat:.4f}", f"{lon:.4f}"])

    print(f"Wrote {len(averaged)} PLZ entries to {OUTPUT_PATH}")
    # Print a few samples
    for entry in averaged[:3]:
        print(f"  {entry[0]}: {entry[1]:.4f}, {entry[2]:.4f}")
    print(f"  ...")
    for entry in averaged[-3:]:
        print(f"  {entry[0]}: {entry[1]:.4f}, {entry[2]:.4f}")


if __name__ == "__main__":
    main()
