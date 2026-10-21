//! Access log and brute-force lockout for incoming control sessions.
//!
//! Every connection attempt (allowed, wrong password, denied, locked) is appended to
//! `%APPDATA%\RemoteDesk\access.log`. Too many wrong passwords from the same peer in a short
//! window locks that peer out for a while, with the delay growing as attempts pile up.

use std::collections::HashMap;
use std::io::Write;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::config::config_dir;

/// Wrong-password attempts allowed before a peer is locked out.
const MAX_ATTEMPTS: u32 = 5;
/// How long the sliding attempt window lasts; older failures are forgotten.
const WINDOW: Duration = Duration::from_secs(300);
/// Base lockout once the limit is hit; doubles for each extra failure, capped at 30 min.
const BASE_LOCKOUT: Duration = Duration::from_secs(30);
const MAX_LOCKOUT: Duration = Duration::from_secs(30 * 60);

struct Attempts {
    failures: Vec<Instant>,
    locked_until: Option<Instant>,
}

static STATE: Mutex<Option<HashMap<String, Attempts>>> = Mutex::new(None);

fn with_state<R>(f: impl FnOnce(&mut HashMap<String, Attempts>) -> R) -> R {
    let mut guard = STATE.lock().unwrap();
    f(guard.get_or_insert_with(HashMap::new))
}

/// If this peer is currently locked out, returns the remaining time.
pub fn lockout_remaining(peer: &str) -> Option<Duration> {
    with_state(|m| {
        m.get(peer).and_then(|a| a.locked_until).and_then(|until| until.checked_duration_since(Instant::now()))
    })
}

/// Records a wrong-password attempt and returns the lockout duration if this one triggers a lock.
pub fn record_failure(peer: &str) -> Option<Duration> {
    with_state(|m| {
        let now = Instant::now();
        let a = m.entry(peer.to_string()).or_insert(Attempts { failures: Vec::new(), locked_until: None });
        a.failures.retain(|t| now.duration_since(*t) < WINDOW);
        a.failures.push(now);
        if a.failures.len() as u32 >= MAX_ATTEMPTS {
            let over = a.failures.len() as u32 - MAX_ATTEMPTS;
            let lockout = (BASE_LOCKOUT * 2u32.saturating_pow(over)).min(MAX_LOCKOUT);
            a.locked_until = Some(now + lockout);
            a.failures.clear();
            Some(lockout)
        } else {
            None
        }
    })
}

/// Clears a peer's failure history after a successful login.
pub fn record_success(peer: &str) {
    with_state(|m| {
        m.remove(peer);
    });
}

fn now_string() -> String {
    // Local wall-clock without pulling in a date crate: derive Y-M-D H:M:S from the timezone
    // offset Windows reports via the environment is unreliable, so log UTC with a Z suffix.
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let days = secs / 86400;
    let (h, mi, s) = ((secs % 86400) / 3600, (secs % 3600) / 60, secs % 60);
    // Civil-from-days (Howard Hinnant's algorithm).
    let z = days as i64 + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02} {h:02}:{mi:02}:{s:02}Z")
}

/// Appends one line to the access log. Best-effort; failures are ignored.
pub fn log(event: &str, peer: &str, name: &str, detail: &str) {
    let path = config_dir().join("access.log");
    if std::fs::metadata(&path).map(|m| m.len() > 2 * 1024 * 1024).unwrap_or(false) {
        // Keep the log from growing without bound.
        let _ = std::fs::rename(&path, path.with_extension("log.1"));
    }
    let _ = std::fs::create_dir_all(config_dir());
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let short = &peer[..peer.len().min(12)];
        let _ = writeln!(f, "{}\t{event}\t{name}\t{short}\t{detail}", now_string());
    }
}
