#!/usr/bin/env -S uv run --script
# /// script
# dependencies = ["plotext"]
# ///
"""Generate a text-based chart of the cone visibility filter for the README."""

import plotext as plt

MAX_DIST = 15.0
MAX_ALT = 35_000.0

# Cone boundary line: max_distance = altitude * 15 / 35000
altitudes = list(range(0, 35_001, 100))
distances = [alt * MAX_DIST / MAX_ALT for alt in altitudes]

# Convert altitudes to flight levels for display (hundreds of feet)
alt_thousands = [alt / 1000 for alt in altitudes]

plt.plot(distances, alt_thousands, label="cone boundary", fillx=True)
plt.title("Cone Visibility Filter")
plt.xlabel("Distance (NM)")
plt.ylabel("Altitude (×1000 ft)")
plt.plot_size(70, 20)
plt.theme("clear")

# Build the plot string without ANSI codes
output = plt.build()
# Strip ANSI escape sequences
import re
clean = re.sub(r'\x1b\[[0-9;]*m', '', output)
print(clean)
