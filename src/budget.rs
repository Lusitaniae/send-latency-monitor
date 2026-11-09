//! The daily spend ledger. A round reserves its worst case before it sends and
//! settles to what it actually paid. The ledger is written to disk at both steps,
//! so a crash leaves the worst case counted: the sender over-counts, never under.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct State {
    /// UTC day, `YYYY-MM-DD`.
    day: String,
    /// Lamports spent or reserved on `day`.
    lamports: u64,
}

pub struct Ledger {
    path: PathBuf,
    daily: u64,
    state: State,
}

/// A reservation for one round, settled with [`Ledger::settle`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reservation {
    day: String,
    lamports: u64,
}

impl Ledger {
    pub fn open(state_dir: &Path, daily: u64) -> Result<Self> {
        std::fs::create_dir_all(state_dir)
            .with_context(|| format!("creating state dir {}", state_dir.display()))?;
        let path = state_dir.join("spend.json");
        let state = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .with_context(|| format!("{} is not a spend ledger", path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => State::default(),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        Ok(Self { path, daily, state })
    }

    fn roll(&mut self, today: &str) {
        if self.state.day != today {
            self.state = State {
                day: today.to_string(),
                lamports: 0,
            };
        }
    }

    pub fn spent(&mut self, today: &str) -> u64 {
        self.roll(today);
        self.state.lamports
    }

    /// Reserve `worst` lamports for a round, or `None` if it would pass the budget.
    pub fn reserve(&mut self, today: &str, worst: u64) -> Result<Option<Reservation>> {
        self.roll(today);
        let after = self.state.lamports.saturating_add(worst);
        if after > self.daily {
            return Ok(None);
        }
        self.state.lamports = after;
        self.save()?;
        Ok(Some(Reservation {
            day: today.to_string(),
            lamports: worst,
        }))
    }

    /// Replace a reservation with what the round actually paid. A reservation from
    /// an earlier day is left counted against that day.
    pub fn settle(&mut self, reservation: Reservation, paid: u64) -> Result<()> {
        if reservation.day == self.state.day {
            let paid = paid.min(reservation.lamports);
            self.state.lamports = self
                .state
                .lamports
                .saturating_sub(reservation.lamports - paid);
            self.save()?;
        }
        Ok(())
    }

    fn save(&self) -> Result<()> {
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec(&self.state)?)
            .with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, &self.path)
            .with_context(|| format!("replacing {}", self.path.display()))
    }
}

pub fn utc_day(now: time::OffsetDateTime) -> String {
    let d = now.date();
    format!("{:04}-{:02}-{:02}", d.year(), u8::from(d.month()), d.day())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("send-bench-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn reserves_until_the_budget_and_settles_down() {
        let dir = dir();
        let mut ledger = Ledger::open(&dir, 100).unwrap();
        let a = ledger.reserve("2026-11-09", 60).unwrap().unwrap();
        assert!(ledger.reserve("2026-11-09", 60).unwrap().is_none());
        ledger.settle(a, 10).unwrap();
        assert_eq!(ledger.spent("2026-11-09"), 10);
        assert!(ledger.reserve("2026-11-09", 60).unwrap().is_some());
        // Survives a restart, reservations included.
        let mut again = Ledger::open(&dir, 100).unwrap();
        assert_eq!(again.spent("2026-11-09"), 70);
    }

    #[test]
    fn a_new_day_starts_at_zero_and_ignores_old_reservations() {
        let dir = dir();
        let mut ledger = Ledger::open(&dir, 100).unwrap();
        let old = ledger.reserve("2026-11-09", 90).unwrap().unwrap();
        assert!(ledger.reserve("2026-11-10", 90).unwrap().is_some());
        ledger.settle(old, 0).unwrap();
        assert_eq!(ledger.spent("2026-11-10"), 90);
    }

    #[test]
    fn paying_more_than_reserved_cannot_lower_the_count() {
        let dir = dir();
        let mut ledger = Ledger::open(&dir, 100).unwrap();
        let r = ledger.reserve("2026-11-09", 50).unwrap().unwrap();
        ledger.settle(r, 80).unwrap();
        assert_eq!(ledger.spent("2026-11-09"), 50);
    }

    #[test]
    fn formats_utc_days() {
        let t = time::OffsetDateTime::from_unix_timestamp(1_794_194_356).unwrap();
        assert_eq!(utc_day(t), "2026-11-09");
    }
}
