use chrono::{DateTime, NaiveDateTime, NaiveTime, TimeDelta, Utc};
use chrono_tz::Tz;
use eyre::{Result, eyre};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Schedule {
    pub name: String,
    pub start_date: Option<NaiveDateTime>,
    pub end_date: Option<NaiveDateTime>,
    pub active_time_start: Option<NaiveTime>,
    pub active_time_end: Option<NaiveTime>,
    pub interval: TimeDelta,
    pub message: String,
}

impl Schedule {
    /// Check if the schedule is currently active based on date range and time window.
    pub fn is_active(&self, now: DateTime<Tz>) -> bool {
        // Check date range (dates are Berlin local time from config)
        if let Some(start) = self.start_date {
            let start_berlin = crate::resolve_berlin_time(start);
            if now < start_berlin {
                return false;
            }
        }

        if let Some(end) = self.end_date {
            let end_berlin = crate::resolve_berlin_time(end);
            if now > end_berlin {
                return false;
            }
        }

        // Check time window (if specified)
        if let (Some(start_time), Some(end_time)) =
            (self.active_time_start, self.active_time_end)
        {
            let current_time = now.time();

            // Handle midnight-spanning windows (e.g., 22:00 - 02:00)
            if end_time < start_time {
                // Window spans midnight: active if time >= start OR time < end
                if !(current_time >= start_time || current_time < end_time) {
                    return false;
                }
            } else {
                // Normal window: active if time is within range
                if !(current_time >= start_time && current_time < end_time) {
                    return false;
                }
            }
        }

        true
    }

    /// Parse interval string into TimeDelta.
    /// Supports formats:
    /// - "hh:mm" (e.g., "01:30" for 1 hour 30 minutes)
    /// - Legacy "30m", "1h", "2h30m" format (backwards compatibility)
    pub fn parse_interval(s: &str) -> Result<TimeDelta> {
        let s = s.trim();
        if s.is_empty() {
            return Err(eyre!("Interval string is empty"));
        }

        // Try parsing as hh:mm format first
        if s.contains(':') {
            let parts: Vec<&str> = s.split(':').collect();
            if parts.len() != 2 {
                return Err(eyre!("Invalid hh:mm format: {}", s));
            }

            let hours: i64 = parts[0]
                .parse()
                .map_err(|_| eyre!("Invalid hours in hh:mm format: {}", parts[0]))?;

            let minutes: i64 = parts[1]
                .parse()
                .map_err(|_| eyre!("Invalid minutes in hh:mm format: {}", parts[1]))?;

            if hours < 0 || !(0..60).contains(&minutes) {
                return Err(eyre!(
                    "Invalid hh:mm values (hours={}, minutes={})",
                    hours,
                    minutes
                ));
            }

            let total_seconds = hours * 3600 + minutes * 60;

            // Enforce minimum interval of 1 minute to prevent spam
            if total_seconds < 60 {
                return Err(eyre!("Interval must be at least 1 minute (got {})", s));
            }

            return TimeDelta::try_seconds(total_seconds)
                .ok_or_else(|| eyre!("Interval too large: {} seconds", total_seconds));
        }

        // Legacy format parsing (e.g., "30m", "1h", "2h30m")
        let s = s.to_lowercase();
        let mut total_seconds = 0i64;
        let mut current_num = String::new();

        for ch in s.chars() {
            if ch.is_ascii_digit() {
                current_num.push(ch);
            } else if ch == 'h' || ch == 'm' || ch == 's' {
                if current_num.is_empty() {
                    return Err(eyre!("No number before unit '{}'", ch));
                }

                let num: i64 = current_num
                    .parse()
                    .map_err(|_| eyre!("Invalid number: {}", current_num))?;

                total_seconds += match ch {
                    'h' => num * 3600,
                    'm' => num * 60,
                    's' => num,
                    _ => unreachable!(),
                };

                current_num.clear();
            } else {
                return Err(eyre!("Invalid character in interval: '{}'", ch));
            }
        }

        if !current_num.is_empty() {
            return Err(eyre!(
                "Number without unit at end of interval: {}",
                current_num
            ));
        }

        if total_seconds == 0 {
            return Err(eyre!("Interval must be greater than zero"));
        }

        // Enforce minimum interval of 1 minute to prevent spam
        if total_seconds < 60 {
            return Err(eyre!(
                "Interval must be at least 1 minute (got {} seconds)",
                total_seconds
            ));
        }

        TimeDelta::try_seconds(total_seconds)
            .ok_or_else(|| eyre!("Interval too large: {} seconds", total_seconds))
    }

    /// Validate the schedule for required fields and logical consistency.
    pub fn validate(&self) -> Result<()> {
        // Name is required and must not be empty
        if self.name.trim().is_empty() {
            return Err(eyre!("Schedule name cannot be empty"));
        }

        // Message is required and must not be empty
        if self.message.trim().is_empty() {
            return Err(eyre!("Schedule message cannot be empty"));
        }

        // Interval must be positive
        if self.interval.num_seconds() <= 0 {
            return Err(eyre!("Interval must be positive"));
        }

        // Interval must be at least 1 minute
        if self.interval.num_seconds() < 60 {
            return Err(eyre!("Interval must be at least 1 minute"));
        }

        // If both start_date and end_date are set, end must be after start
        if let (Some(start), Some(end)) = (self.start_date, self.end_date)
            && end <= start
        {
            return Err(eyre!("End date must be after start date"));
        }

        // Time window validation: both or neither must be set
        match (self.active_time_start, self.active_time_end) {
            (Some(_), None) => {
                return Err(eyre!(
                    "active_time_end must be set if active_time_start is set"
                ));
            }
            (None, Some(_)) => {
                return Err(eyre!(
                    "active_time_start must be set if active_time_end is set"
                ));
            }
            _ => {} // Both set or both None is valid
        }

        Ok(())
    }
}

/// Cache structure for storing loaded schedules with metadata.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ScheduleCache {
    pub schedules: Vec<Schedule>,
    pub last_updated: DateTime<Utc>,
    pub version: u64,
}

impl ScheduleCache {
    /// Create a new empty cache.
    pub fn new() -> Self {
        Self {
            schedules: Vec::new(),
            last_updated: Utc::now(),
            version: 0,
        }
    }

    /// Update cache with new schedules, incrementing version.
    pub fn update(&mut self, schedules: Vec<Schedule>) {
        self.schedules = schedules;
        self.last_updated = Utc::now();
        self.version += 1;
    }
}
