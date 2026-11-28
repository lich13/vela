//! How fast the clients are spending right now, as a speedometer has it and
//! not as the average of a period.
//!
//! A request moves the dial when it ends, which is when its cost is known:
//! most at first, and less the longer ago it was, until `WINDOW` later it no
//! longer does. The weight of an age falls off as `exp(-age / FADE)`, lowered
//! so that it reaches nothing where the window ends, and over the window it
//! adds up to one: spending that holds steady reads as itself, and whatever
//! was spent is shown for as much as it was. A request ran its cost up while
//! it was answered, so it is weighed over that stretch and not at its end,
//! and a long answer does not read as though it had been spent at once.
//!
//! What a gateway recorded is known as it ends. What is read from a client's
//! sessions is known a reading later, and all of a minute at once: weighed
//! at the present, spending that held steady would fall to a third between
//! two readings and leap back with the next. So it is weighed at the moment
//! its sessions were last read, and stands there until they are read again;
//! sessions that go unread stand for `HOLD` and fade from then on.
use super::{antigravity::classify_model_family, storage_v12, store::db};
use crate::storage::Result;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::Path};

/// How far back a request still moves the dial, in milliseconds.
const WINDOW: f64 = 300_000.0;
/// How long a request takes to fall to about a third of what it first read.
const FADE: f64 = 60_000.0;
/// How long the last reading of a client's sessions stands for the present,
/// in milliseconds: the minute until the next and what that takes to read.
const HOLD: i64 = 90_000;
const HOUR: f64 = 3_600_000.0;
/// The reading the dial ends at, in USD per hour.
const FULL: f64 = 200.0;
/// The reading about which the dial's scale turns from even to logarithmic:
/// a few cents an hour still move the needle, and a hundred dollars fit.
const KNEE: f64 = 0.25;
/// The readings the dial is labelled at.
const MARKS: [u32; 4] = [1, 5, 20, 100];
/// The clients a reading is given for, in the order they are shown in.
const CLIENTS: [&str; 5] = ["codex", "claude", "antigravity", "grok", "dsh"];

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Rate {
    /// USD per hour, written like a cost; "0" when idle.
    pub cost_per_hour: String,
    /// How far up the dial that is, from 0 to 1.
    pub level: f64,
    /// Tokens are being used and none of them could be priced.
    pub unpriced: bool,
}
/// The reading of one of several: a client, or a quota track of an account.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Named {
    pub id: String,
    #[serde(flatten)]
    pub rate: Rate,
}
/// `label` USD per hour stands at `level` of the dial.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Mark {
    pub label: String,
    pub level: f64,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Account {
    pub id: String,
    pub tracks: Vec<Named>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Speed {
    /// Every client taken together, whether or not it is one of `clients`.
    #[serde(flatten)]
    pub total: Rate,
    /// When it was read, in Unix milliseconds.
    pub at: i64,
    pub dial: Vec<Mark>,
    /// Every client of `CLIENTS`, in that order, idle or not.
    pub clients: Vec<Named>,
    /// Antigravity by account and quota track; only those with a request in
    /// the window.
    pub accounts: Vec<Account>,
}

/// What of a cost the dial shows, per millisecond, when the request ended
/// `age` milliseconds ago and took `took` of them.
fn share(age: f64, took: f64) -> f64 {
    let edge = (-WINDOW / FADE).exp();
    let whole = FADE * (1.0 - edge) - WINDOW * edge;
    // The weights of every age up to this one, added up.
    let gathered = |age: f64| {
        let age = age.clamp(0.0, WINDOW);
        (FADE * (1.0 - (-age / FADE).exp()) - age * edge) / whole
    };
    let age = age.max(0.0);
    if took >= 1.0 {
        (gathered(age + took) - gathered(age)) / took
    } else if age < WINDOW {
        ((-age / FADE).exp() - edge) / whole
    } else {
        0.0
    }
}

/// How far up the dial `per_hour` stands.
fn level(per_hour: f64) -> f64 {
    let level = (1.0 + per_hour / KNEE).ln() / (1.0 + FULL / KNEE).ln();
    (level.clamp(0.0, 1.0) * 10_000.0).round() / 10_000.0
}

#[derive(Clone, Copy, Default)]
struct Spent {
    per_ms: f64,
    unpriced: bool,
}
impl Spent {
    fn add(&mut self, per_ms: f64, unpriced: bool) {
        self.per_ms += per_ms;
        self.unpriced |= unpriced;
    }
    fn rate(self) -> Rate {
        // To the ten-thousandth of a dollar, as a cost is written.
        let per_hour = (self.per_ms * HOUR * 10_000.0).round() / 10_000.0;
        let written = format!("{per_hour:.4}");
        Rate {
            cost_per_hour: written
                .trim_end_matches('0')
                .trim_end_matches('.')
                .to_owned(),
            level: level(per_hour),
            unpriced: self.unpriced && per_hour == 0.0,
        }
    }
}

/// The reading at `now`. `sessions` is when the reading of each client's
/// sessions that last came to an end began, by the client's name. `data` is
/// where the Antigravity accounts are kept: a request of Antigravity's that
/// names no account is the one's that was signed in when it ended, as in the
/// account statistics.
pub(super) fn read(
    c: &Connection,
    data: Option<&Path>,
    now: i64,
    sessions: &BTreeMap<String, i64>,
) -> Result<Speed> {
    let mut statement = db(c.prepare_cached(
        "SELECT usage_label(r.client),usage_value(p.value),usage_value(m.value),
         CASE WHEN r.client=usage_label('antigravity') THEN s.value END,a.cost,
         coalesce(a.unpriced>0 AND coalesce(a.input,0)+coalesce(a.output,0)+coalesce(a.cache_read,0)+coalesce(a.cache_write,0)>0,0),
         a.duration,r.time,r.source<>0
         FROM v12_records r JOIN v12_all_attempts a ON a.owner=r.rid
         LEFT JOIN v12_dictionary p ON p.id=a.provider
         LEFT JOIN v12_dictionary m ON m.id=a.model
         LEFT JOIN v12_dictionary s ON s.id=a.shape
         WHERE r.effective=1 AND r.time>?1 AND r.time<=?2",
    ))?;
    let mut rows = db(statement.query(params![now - HOLD - WINDOW as i64, now]))?;
    let mut total = Spent::default();
    let mut clients = [Spent::default(); CLIENTS.len()];
    let mut accounts: BTreeMap<String, BTreeMap<&'static str, Spent>> = BTreeMap::new();
    // Read where a request needs it, and then once.
    let mut timeline = None;
    while let Some(row) = db(rows.next())? {
        let client: String = db(row.get(0))?;
        let ended: i64 = db(row.get(7))?;
        // The moment it is weighed at: one it had not ended by does not know
        // of it yet, and one a window later no longer does.
        let known = if db(row.get(8))? {
            let read = sessions.get(&client).copied().unwrap_or(i64::MIN);
            read.clamp(now - HOLD, now)
        } else {
            now
        };
        if ended > known || known - ended >= WINDOW as i64 {
            continue;
        }
        let took = db(row.get::<_, Option<i64>>(6))?.unwrap_or(0).max(0);
        let cost = db(row.get::<_, Option<String>>(4))?
            .and_then(|cost| cost.parse::<f64>().ok())
            .filter(|cost| cost.is_finite() && *cost > 0.0)
            .unwrap_or(0.0);
        let per_ms = cost * share((known - ended) as f64, took as f64);
        let unpriced: bool = db(row.get(5))?;
        total.add(per_ms, unpriced);
        let Some(index) = CLIENTS.iter().position(|id| *id == client) else {
            continue;
        };
        clients[index].add(per_ms, unpriced);
        if client != "antigravity" {
            continue;
        }
        let model: Option<String> = db(row.get(2))?;
        // The name it is shown by tells the track of a model with no id.
        let shown = if model.as_deref().is_none_or(|model| model.trim().is_empty()) {
            db(row.get::<_, Option<Vec<u8>>>(3))?
                .as_deref()
                .and_then(storage_v12::shared_value)
                .and_then(|shape| shape["modelDisplayName"].as_str().map(str::to_owned))
        } else {
            None
        };
        let Some(track) = classify_model_family(model.as_deref(), shown.as_deref()).3 else {
            continue;
        };
        let account = match db(row.get::<_, Option<String>>(1))? {
            Some(account) => Some(account),
            None => timeline
                .get_or_insert_with(|| {
                    data.map(|data| crate::antigravity::auth::provider_timeline(data, None))
                        .unwrap_or_default()
                })
                .provider_at(ended)
                .map(str::to_owned),
        };
        if let Some(account) = account {
            accounts
                .entry(account)
                .or_default()
                .entry(track)
                .or_default()
                .add(per_ms, unpriced);
        }
    }
    Ok(Speed::of(now, total, clients, accounts))
}

impl Speed {
    /// The reading of nothing being spent: what a dial shows before it has
    /// been read.
    #[cfg(any(all(feature = "desktop", target_os = "macos"), test))]
    pub(crate) fn idle(at: i64) -> Self {
        Self::of(at, Spent::default(), Default::default(), BTreeMap::new())
    }

    fn of(
        at: i64,
        total: Spent,
        clients: [Spent; CLIENTS.len()],
        accounts: BTreeMap<String, BTreeMap<&'static str, Spent>>,
    ) -> Self {
        let named = |id: &str, spent: Spent| Named {
            id: id.into(),
            rate: spent.rate(),
        };
        Self {
            total: total.rate(),
            at,
            dial: MARKS
                .iter()
                .map(|mark| Mark {
                    label: mark.to_string(),
                    level: level(f64::from(*mark)),
                })
                .collect(),
            clients: CLIENTS
                .iter()
                .zip(clients)
                .map(|(id, spent)| named(id, spent))
                .collect(),
            accounts: accounts
                .into_iter()
                .map(|(id, tracks)| Account {
                    id,
                    tracks: tracks
                        .into_iter()
                        .map(|(track, spent)| named(track, spent))
                        .collect(),
                })
                .collect(),
        }
    }
}

#[cfg(test)]
#[path = "speed_tests.rs"]
mod tests;
