#!/usr/bin/env bash
# Downloads OPTD airline data and extracts IATA,ICAO mappings.
# Only includes active airlines (no validity_to date) with both codes present.
set -euo pipefail

OPTD_URL="https://raw.githubusercontent.com/opentraveldata/opentraveldata/master/opentraveldata/optd_airline_best_known_so_far.csv"

curl -sL "$OPTD_URL" \
  | awk -F'^' '
    NR == 1 { next }
    # $4 = 3char_code (ICAO), $5 = 2char_code (IATA), $4_col = validity_to is $4...
    # Actually columns: pk(1) env_id(2) validity_from(3) validity_to(4) 3char_code(5) 2char_code(6)
    {
      validity_to = $4
      icao = $5
      iata = $6
      # Only active airlines (no end date) with both codes present
      if (validity_to == "" && iata != "" && icao != "" && length(iata) == 2 && length(icao) == 3)
        print iata "," icao
    }
  ' \
  | sort \
  | awk -F',' '!seen[$1]++' \
  > crates/core/data/airlines.csv

echo "Generated crates/core/data/airlines.csv with $(wc -l < crates/core/data/airlines.csv) entries"
