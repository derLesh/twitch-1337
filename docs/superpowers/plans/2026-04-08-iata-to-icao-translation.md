# IATA-to-ICAO Flight Number Translation — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Translate IATA flight numbers (e.g., `TP247`) to ICAO callsigns (e.g., `TAP247`) so `!track` works with both formats.

**Architecture:** Static embedded CSV for IATA-to-ICAO airline code lookup, with async adsbdb API fallback. New `resolve_callsign()` method on `AviationClient`. Single integration point in `flight_tracker.rs` before the adsb.lol query.

**Tech Stack:** Rust, `include_str!` + `OnceLock` for static data, adsbdb `/v0/airline/` endpoint for fallback.

**Spec:** `docs/superpowers/specs/2026-04-08-iata-to-icao-translation-design.md`

---

### Task 1: Create the airline data file

**Files:**
- Create: `scripts/generate_airlines_csv.sh`
- Create: `data/airlines.csv`

- [ ] **Step 1: Write the data extraction script**

Create `scripts/generate_airlines_csv.sh` to download the OPTD dataset and extract a simple 2-column CSV:

```bash
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
  | sort -u \
  > data/airlines.csv

echo "Generated data/airlines.csv with $(wc -l < data/airlines.csv) entries"
```

- [ ] **Step 2: Run the script to generate the data file**

Run: `bash scripts/generate_airlines_csv.sh`

Expected: `data/airlines.csv` created with ~800-1200 entries. Verify with:

```bash
head -5 data/airlines.csv
# Expected format:
# AA,AAL
# BA,BAW
# LH,DLH
# TP,TAP
# ...
```

Verify the specific mapping from the bug report:

```bash
grep "^TP," data/airlines.csv
# Expected: TP,TAP
```

- [ ] **Step 3: Commit**

```bash
git add scripts/generate_airlines_csv.sh data/airlines.csv
git commit -m "feat: add IATA-to-ICAO airline code data from OPTD"
```

---

### Task 2: Add airline code lookup to `aviation.rs`

**Files:**
- Modify: `src/aviation.rs`

This task adds the static CSV loading (same pattern as PLZ and airport data) and the `resolve_callsign()` method with adsbdb API fallback.

- [ ] **Step 1: Add the static airline table**

Add the following after the airport lookup section (after line 108 in `src/aviation.rs`, before `fn is_icao_pattern`):

```rust
// --- Airline IATA-to-ICAO Lookup ---

const AIRLINE_DATA: &str = include_str!("../data/airlines.csv");

/// Returns the static IATA→ICAO airline code table (lazy-initialized).
fn airline_table() -> &'static HashMap<&'static str, &'static str> {
    static TABLE: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut map = HashMap::new();
        for line in AIRLINE_DATA.lines() {
            let Some((iata, icao)) = line.split_once(',') else {
                continue;
            };
            let iata = iata.trim();
            let icao = icao.trim();
            if iata.len() == 2 && icao.len() == 3 {
                map.insert(iata, icao);
            }
        }
        map
    })
}

/// Check if input looks like an IATA flight number (2 letters + 1-4 digits).
fn is_iata_flight_number(s: &str) -> bool {
    let bytes = s.as_bytes();
    if bytes.len() < 3 || bytes.len() > 6 {
        return false;
    }
    bytes[0].is_ascii_uppercase()
        && bytes[1].is_ascii_uppercase()
        && bytes[2..].iter().all(|b| b.is_ascii_digit())
}
```

- [ ] **Step 2: Add the adsbdb airline response type**

Add after the existing adsbdb types section (after line 191, near the `AdsbDbResponseInner` struct):

```rust
// --- adsbdb airline types ---

#[derive(Debug, Deserialize)]
struct AdsbDbAirlineResponse {
    response: Vec<AdsbDbAirline>,
}

#[derive(Debug, Deserialize)]
struct AdsbDbAirline {
    icao: String,
}
```

- [ ] **Step 3: Add a timeout constant for the airline API call**

Add near the other timeout constants at the top of the file (after line 19):

```rust
const AIRLINE_LOOKUP_TIMEOUT: Duration = Duration::from_secs(5);
```

- [ ] **Step 4: Add `resolve_callsign()` method to `AviationClient`**

Add this method inside the `impl AviationClient` block (after the `get_flight_route` method, around line 315):

```rust
    /// Resolve a potential IATA flight number to an ICAO callsign.
    ///
    /// If input matches the IATA pattern (2 letters + digits), attempts translation
    /// via embedded CSV first, then adsbdb API fallback. Returns input unchanged if
    /// not an IATA pattern or if translation fails.
    pub(crate) async fn resolve_callsign(&self, input: &str) -> String {
        if !is_iata_flight_number(input) {
            return input.to_string();
        }

        let (airline_iata, flight_num) = input.split_at(2);

        // Try static CSV lookup first
        if let Some(&icao) = airline_table().get(airline_iata) {
            debug!(iata = %airline_iata, icao = %icao, "Resolved airline code via CSV");
            return format!("{icao}{flight_num}");
        }

        // Fallback: query adsbdb airline API
        debug!(iata = %airline_iata, "Airline not in CSV, trying adsbdb API");
        match tokio::time::timeout(
            AIRLINE_LOOKUP_TIMEOUT,
            self.lookup_airline_icao(airline_iata),
        )
        .await
        {
            Ok(Ok(Some(icao))) => {
                warn!(
                    iata = %airline_iata,
                    icao = %icao,
                    "Resolved airline via adsbdb API — consider adding to airlines.csv"
                );
                format!("{icao}{flight_num}")
            }
            Ok(Ok(None)) => {
                debug!(iata = %airline_iata, "Airline not found in adsbdb");
                input.to_string()
            }
            Ok(Err(e)) => {
                warn!(error = ?e, iata = %airline_iata, "adsbdb airline lookup failed");
                input.to_string()
            }
            Err(_) => {
                warn!(iata = %airline_iata, "adsbdb airline lookup timed out");
                input.to_string()
            }
        }
    }

    /// Query adsbdb for an airline's ICAO code by IATA code.
    async fn lookup_airline_icao(&self, iata: &str) -> Result<Option<String>> {
        let url = format!("{ADSBDB_BASE_URL}/airline/{iata}");
        debug!(url = %url, "Fetching airline from adsbdb");

        let resp = self
            .0
            .get(&url)
            .send()
            .await
            .wrap_err("Failed to send request to adsbdb")?;

        if !resp.status().is_success() {
            return Ok(None);
        }

        let body: AdsbDbAirlineResponse = resp
            .json()
            .await
            .wrap_err("Failed to parse adsbdb airline response")?;

        Ok(body.response.into_iter().next().map(|a| a.icao))
    }
```

- [ ] **Step 5: Verify it compiles**

Run: `cargo check`
Expected: Compiles with no errors. There may be dead-code warnings for the new functions since they're not called yet — that's fine.

- [ ] **Step 6: Commit**

```bash
git add src/aviation.rs
git commit -m "feat: add IATA-to-ICAO airline code resolution to AviationClient

Adds resolve_callsign() with embedded CSV lookup and adsbdb API fallback.
Closes #1"
```

---

### Task 3: Integrate into flight tracker

**Files:**
- Modify: `src/flight_tracker.rs:706-708`

- [ ] **Step 1: Add the translation call before the adsb.lol query**

In `src/flight_tracker.rs`, find the callsign branch of the match (around line 706):

```rust
        FlightIdentifier::Callsign(cs) => {
            tokio::time::timeout(POLL_TIMEOUT, aviation_client.get_aircraft_by_callsign(cs)).await
        }
```

Replace with:

```rust
        FlightIdentifier::Callsign(cs) => {
            let resolved = aviation_client.resolve_callsign(cs).await;
            tokio::time::timeout(
                POLL_TIMEOUT,
                aviation_client.get_aircraft_by_callsign(&resolved),
            )
            .await
        }
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo check`
Expected: Compiles with no errors and no dead-code warnings for the new functions.

- [ ] **Step 3: Verify clippy passes**

Run: `cargo clippy`
Expected: No new warnings.

- [ ] **Step 4: Commit**

```bash
git add src/flight_tracker.rs
git commit -m "feat: translate IATA flight numbers before adsb.lol lookup

Calls resolve_callsign() to convert IATA flight numbers (e.g., TP247)
to ICAO callsigns (e.g., TAP247) before querying adsb.lol."
```

---

### Task 4: Add unit tests

**Files:**
- Modify: `src/aviation.rs` (add `#[cfg(test)]` module at end of file)

- [ ] **Step 1: Add test module with airline table and pattern detection tests**

Add at the end of `src/aviation.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn airline_table_contains_known_mappings() {
        let table = airline_table();
        assert_eq!(table.get("TP"), Some(&"TAP"));
        assert_eq!(table.get("LH"), Some(&"DLH"));
        assert_eq!(table.get("BA"), Some(&"BAW"));
    }

    #[test]
    fn airline_table_is_nonempty() {
        assert!(airline_table().len() > 100);
    }

    #[test]
    fn is_iata_flight_number_valid() {
        assert!(is_iata_flight_number("TP247"));
        assert!(is_iata_flight_number("LH5765"));
        assert!(is_iata_flight_number("BA12"));
        assert!(is_iata_flight_number("AA1"));
        assert!(is_iata_flight_number("EI1234"));
    }

    #[test]
    fn is_iata_flight_number_rejects_icao() {
        assert!(!is_iata_flight_number("TAP247"));
        assert!(!is_iata_flight_number("DLH5765"));
        assert!(!is_iata_flight_number("BAW12"));
    }

    #[test]
    fn is_iata_flight_number_rejects_invalid() {
        assert!(!is_iata_flight_number(""));
        assert!(!is_iata_flight_number("T"));
        assert!(!is_iata_flight_number("TP"));
        assert!(!is_iata_flight_number("12345"));
        assert!(!is_iata_flight_number("ABCDEF"));
        assert!(!is_iata_flight_number("TP12345")); // too many digits
    }

    #[tokio::test]
    async fn resolve_callsign_translates_iata() {
        let client = AviationClient::new().unwrap();
        assert_eq!(client.resolve_callsign("TP247").await, "TAP247");
        assert_eq!(client.resolve_callsign("LH5765").await, "DLH5765");
    }

    #[tokio::test]
    async fn resolve_callsign_passes_through_icao() {
        let client = AviationClient::new().unwrap();
        assert_eq!(client.resolve_callsign("TAP247").await, "TAP247");
        assert_eq!(client.resolve_callsign("DLH5765").await, "DLH5765");
    }

    #[tokio::test]
    async fn resolve_callsign_passes_through_hex() {
        let client = AviationClient::new().unwrap();
        assert_eq!(client.resolve_callsign("4CA87D").await, "4CA87D");
    }
}
```

- [ ] **Step 2: Run the tests**

Run: `cargo test`
Expected: All tests pass.

- [ ] **Step 3: Run clippy one final time**

Run: `cargo clippy`
Expected: No warnings.

- [ ] **Step 4: Commit**

```bash
git add src/aviation.rs
git commit -m "test: add unit tests for airline code translation"
```
