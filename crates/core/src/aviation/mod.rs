//! Aviation features: ADS-B + flight metadata clients, location resolution,
//! flight tracking, and `!up`/`!fl`/`!flight`/`!flights` commands.
//!
//! Submodules:
//! - `client`: HTTP clients (adsb.lol, adsbdb, Nominatim, Aviationstack).
//! - `commands`: command handlers (`up`, `random_flight`, `track`, etc).
//! - `formatting`: shared output formatters.
//! - `location`: embedded data tables + IATA/ICAO predicates + resolver.
//! - `tracker`: long-running flight tracker task.
//! - `types`: API response types.

mod client;
pub mod commands;
mod formatting;
mod location;
pub mod tracker;
mod types;

pub use client::AviationClient;
pub use commands::up::up_command;
pub use tracker::{FlightIdentifier, TrackerCommand, run_flight_tracker};
pub use types::{Airport, AltBaro, AviationstackFlightMetadata, FlightRoute, NearbyAircraft};

pub(crate) use location::iata_to_coords;
