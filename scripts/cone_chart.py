#!/usr/bin/env -S uv run --script
# /// script
# dependencies = ["plotille"]
# ///
"""Generate a text-based chart of the cone visibility filter for the README."""

import re
import plotille

MAX_DIST = 15.0
MAX_ALT = 35_000.0

# Cone boundary line: max_distance = altitude * 15 / 35000
altitudes = list(range(0, 35_001, 100))
distances = [alt * MAX_DIST / MAX_ALT for alt in altitudes]
alt_thousands = [alt / 1000 for alt in altitudes]

fig = plotille.Figure()
fig.width = 60
fig.height = 7  # 35/7 = 5, so Y ticks at 0, 5, 10, 15, 20, 25, 30, 35
fig.x_label = "Distance (NM)"
fig.y_label = "Altitude (×1000 ft)"
fig.set_x_limits(min_=0, max_=15)
fig.set_y_limits(min_=0, max_=35)
fig.plot(distances, alt_thousands, label="cone boundary")

# Strip ANSI color codes for clean README output
output = fig.show(legend=False)
print(re.sub(r'\x1b\[[0-9;]*m', '', output))
