//! Hybrid logical clock (HLC): the order of changes across machines without trusting each one's clock.
//!
//! A stamp is `<milliseconds>-<counter>-<machine>`. Two stamps are compared by their parts (time,
//! counter and machine), never as text: that way old stamps (4-digit counter) and new ones (10
//! digits) sort correctly. Each local change takes a stamp greater than every one this machine has seen
//! (its own and those that came from others) and than that of the memory being changed: if a machine's
//! clock is behind, its changes still win over what it already knew, and the counter breaks ties within
//! the same millisecond. With equal time and counter the machine name decides, so everyone picks the
//! same version. A stamp from another machine that is more than an hour in the future does not advance
//! the local clock (a machine with the wrong time doesn't leave everyone else stuck on its millisecond).

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use std::cmp::Ordering;

/// How far a remote stamp may be ahead of the local time and still advance the clock.
const MAX_FUTURE_MS: u64 = 3_600_000;

/// The system name (for the machine name), asked of the operating system: macOS has no
/// `/etc/hostname` nor an exported `HOSTNAME`, and without it every Mac would have the same name.
fn system_name() -> String {
    std::env::var("OMOIKANE_EQUIPO")
        .ok()
        .filter(|n| !n.trim().is_empty())
        .or_else(os_hostname)
        .or_else(|| std::env::var("HOSTNAME").ok())
        .or_else(|| std::fs::read_to_string("/etc/hostname").ok())
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "equipo".into())
}

#[cfg(unix)]
fn os_hostname() -> Option<String> {
    let mut b = [0u8; 256];
    // SAFETY: gethostname writes at most `b.len()` bytes into the buffer it is given.
    let r = unsafe { libc::gethostname(b.as_mut_ptr().cast::<libc::c_char>(), b.len()) };
    if r != 0 {
        return None;
    }
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    let n = String::from_utf8_lossy(&b[..end]).trim().to_string();
    (!n.is_empty()).then_some(n)
}

#[cfg(windows)]
fn os_hostname() -> Option<String> {
    // Windows always defines it, with the machine's NetBIOS name.
    std::env::var("COMPUTERNAME").ok().filter(|n| !n.trim().is_empty())
}

#[cfg(not(any(unix, windows)))]
fn os_hostname() -> Option<String> {
    None
}

/// This machine's name: the system's one plus a fixed suffix, stored in the database. If the database was
/// copied to another machine (the system is no longer the one recorded), a new name is generated: two
/// machines with the same name would write the same file.
pub fn machine_name(db: &Connection) -> Result<String> {
    let host = system_name().trim().to_lowercase();
    let saved: Option<String> = db.query_row("SELECT v FROM meta WHERE k = 'equipo'", [], |f| f.get(0)).optional()?;
    let saved_host: Option<String> = db.query_row("SELECT v FROM meta WHERE k = 'equipo_host'", [], |f| f.get(0)).optional()?;
    if let Some(e) = saved {
        match saved_host {
            Some(h) if h == host => return Ok(e),
            // Databases from before the system was recorded: record it now and keep the name.
            None => {
                db.execute("INSERT OR REPLACE INTO meta(k, v) VALUES ('equipo_host', ?1)", [&host])?;
                return Ok(e);
            }
            Some(_) => {}
        }
    }
    let clean: String = host.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    let suffix = ulid::Ulid::generate().to_string()[20..].to_lowercase();
    let clean = clean.trim_matches('-');
    let e = format!("{}-{suffix}", if clean.is_empty() { "equipo" } else { clean });
    db.execute("INSERT OR REPLACE INTO meta(k, v) VALUES ('equipo', ?1)", [&e])?;
    db.execute("INSERT OR REPLACE INTO meta(k, v) VALUES ('equipo_host', ?1)", [&host])?;
    Ok(e)
}

fn now_ms() -> u64 {
    let ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64);
    // For testing skewed clocks.
    let skew: i64 = std::env::var("OMOIKANE_RELOJ_DESFASE_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(0);
    (ms + skew).max(0) as u64
}

/// Milliseconds, counter and machine of a stamp (0, 0, "" if it is empty or malformed).
pub fn parts(stamp: &str) -> (u64, u64, &str) {
    let mut p = stamp.splitn(3, '-');
    let ms = p.next().and_then(|x| x.parse().ok()).unwrap_or(0);
    let c = p.next().and_then(|x| x.parse().ok()).unwrap_or(0);
    (ms, c, p.next().unwrap_or(""))
}

/// Compares two stamps by their parts.
pub fn compare(a: &str, b: &str) -> Ordering {
    parts(a).cmp(&parts(b))
}

/// `a` is later than `b`.
pub fn is_after(a: &str, b: &str) -> bool {
    compare(a, b) == Ordering::Greater
}

pub fn format_stamp(ms: u64, counter: u64, machine: &str) -> String {
    format!("{ms:013}-{counter:010}-{machine}")
}

/// A stamp with a counter of another width (those of schema 5 had 4 digits), in the current
/// format; `None` if it already is or it is not a stamp.
pub fn normalized(stamp: &str) -> Option<String> {
    let mut p = stamp.splitn(3, '-');
    let (ms, c, machine) = (p.next()?, p.next()?, p.next().unwrap_or(""));
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if !digits(ms) || !digits(c) || (ms.len() == 13 && c.len() == 10) {
        return None;
    }
    Some(format_stamp(ms.parse().ok()?, c.parse().ok()?, machine))
}

fn last(db: &Connection) -> Result<String> {
    Ok(db.query_row("SELECT v FROM meta WHERE k = 'hlc'", [], |f| f.get(0)).optional()?.unwrap_or_default())
}

/// A new stamp for a change on this machine (inside the change's transaction).
pub fn stamp(db: &Connection) -> Result<String> {
    stamp_after(db, "")
}

/// A new stamp, also later than `prev` (the stamp of the memory being changed or forgotten): a
/// change made here after seeing a version always wins over it, even if that version comes from a machine
/// whose clock is ahead. But a `prev` more than an hour in the future does not advance the clock (as
/// in `receive`): this change wins over it, and the next ones from this machine stay on the local time.
pub fn stamp_after(db: &Connection, prev: &str) -> Result<String> {
    let machine = machine_name(db)?;
    let u = last(db)?;
    let (mut ms, mut c, _) = parts(&u);
    let (ms_p, c_p, _) = parts(prev);
    let now = now_ms();
    let far = ms_p > now.saturating_add(MAX_FUTURE_MS);
    if !far && (ms_p, c_p) > (ms, c) {
        (ms, c) = (ms_p, c_p);
    }
    let (ms, c) = if now > ms { (now, 0) } else { (ms, c + 1) };
    let s = format_stamp(ms, c, &machine);
    db.execute("INSERT OR REPLACE INTO meta(k, v) VALUES ('hlc', ?1)", params![s])?;
    if far && (ms_p, c_p) >= (ms, c) {
        // Right after the previous one, without recording it as the clock's last.
        return Ok(format_stamp(ms_p, c_p + 1, &machine));
    }
    Ok(s)
}

/// Whether a stamp is more than an hour ahead of the local time.
pub fn is_from_future(stamp: &str) -> bool {
    parts(stamp).0 > now_ms().saturating_add(MAX_FUTURE_MS)
}

/// Records a stamp that came from another machine: local changes will come after it. One more than an
/// hour in the future is not recorded (returns `false`).
pub fn receive(db: &Connection, remote: &str) -> Result<bool> {
    let (ms_r, c_r, _) = parts(remote);
    if is_from_future(remote) {
        return Ok(false);
    }
    let (ms_u, c_u, _) = parts(&last(db)?);
    if (ms_r, c_r) > (ms_u, c_u) {
        let machine = machine_name(db)?;
        db.execute("INSERT OR REPLACE INTO meta(k, v) VALUES ('hlc', ?1)", params![format_stamp(ms_r, c_r, &machine)])?;
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compared_by_parts() {
        assert!(is_after(&format_stamp(5, 1, "a"), &format_stamp(5, 0, "b")));
        assert!(is_after(&format_stamp(6, 0, "a"), &format_stamp(5, 9, "z")));
        assert!(is_after(&format_stamp(5, 1, "b"), &format_stamp(5, 1, "a")), "a igual tiempo decide el equipo");
        assert_eq!(parts(&format_stamp(1234, 7, "pc-x")), (1234, 7, "pc-x"));
        assert_eq!(parts(""), (0, 0, ""));
        // An old stamp (4-digit counter) against a new one (10 digits): the number rules.
        assert!(is_after(&format_stamp(5, 10_000, "a"), "0000000000005-9999-a"));
        assert!(is_after("0000000000005-0002-a", &format_stamp(5, 1, "a")));
        assert!(!is_after("0000000000005-0001-a", &format_stamp(5, 1, "a")));
    }

    #[test]
    fn old_stamps_to_current_format() {
        assert_eq!(normalized("1793189415788-0000-").as_deref(), Some("1793189415788-0000000000-"));
        assert_eq!(normalized("1793189415788-0012-pc-a1b2").as_deref(), Some("1793189415788-0000000012-pc-a1b2"));
        assert_eq!(normalized(&format_stamp(5, 3, "x")), None, "ya está bien");
        assert_eq!(normalized(""), None);
        assert_eq!(normalized("basura"), None);
        // Comparing as text is already comparing stamps.
        let (a, b) = (normalized("1793189415788-0002-a").unwrap(), format_stamp(1793189415788, 10, "a"));
        assert_eq!(a < b, compare(&a, &b) == Ordering::Less);
    }

    #[test]
    fn the_future_does_not_advance_the_clock() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE meta (k TEXT PRIMARY KEY, v TEXT NOT NULL);").unwrap();
        let far = format_stamp(now_ms() + 10 * MAX_FUTURE_MS, 0, "otro");
        assert!(!receive(&db, &far).unwrap());
        let s = stamp(&db).unwrap();
        assert!(parts(&s).0 <= now_ms() + 1000, "el reloj sigue en la hora de aquí: {s}");
        // But a change on a memory with that stamp still wins over it, without advancing the clock: the
        // next stamp goes back to the local time.
        let after = stamp_after(&db, &far).unwrap();
        assert!(is_after(&after, &far));
        assert!(is_after(&stamp_after(&db, &after).unwrap(), &after), "y otro cambio sobre esa misma memoria, también");
        let s = stamp(&db).unwrap();
        assert!(parts(&s).0 <= now_ms() + 1000, "el reloj no se envenena: {s}");
        // A nearby one is recorded.
        let near = format_stamp(now_ms() + 60_000, 3, "otro");
        assert!(receive(&db, &near).unwrap());
        assert!(is_after(&stamp(&db).unwrap(), &near));
    }

    #[test]
    fn database_copied_to_another_machine_changes_name() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE meta (k TEXT PRIMARY KEY, v TEXT NOT NULL);").unwrap();
        let a = machine_name(&db).unwrap();
        assert_eq!(machine_name(&db).unwrap(), a);
        db.execute("UPDATE meta SET v = 'otro-sistema' WHERE k = 'equipo_host'", []).unwrap();
        let b = machine_name(&db).unwrap();
        assert_ne!(a, b);
        assert_eq!(machine_name(&db).unwrap(), b);
    }
}
