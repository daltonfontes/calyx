//! Run directories: `.calyx/runs/<id>/` under the current directory, each
//! with the journal the runtime writes (decisions D6, D14).

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

pub const RUNS_DIR: &str = ".calyx/runs";
/// How the runtime says a run stopped to wait for messages (decision D21).
pub const WAITING: &str = "waiting: ";
const JOURNAL: &str = "journal.jsonl";

pub fn dir(id: &str) -> PathBuf {
    Path::new(RUNS_DIR).join(id)
}

/// `YYYYMMDD-HHMMSS-xxxx` (UTC): sorts by start time, unique enough for
/// one machine.
pub fn new_id() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs() as i64;
    let (y, m, d) = civil_from_days(secs.div_euclid(86_400));
    let t = secs.rem_euclid(86_400);
    let salt = (now.subsec_nanos() ^ std::process::id().wrapping_mul(2_654_435_761)) & 0xffff;
    format!(
        "{y:04}{m:02}{d:02}-{:02}{:02}{:02}-{salt:04x}",
        t / 3600,
        t / 60 % 60,
        t % 60
    )
}

/// A new id whose run directory this process has created, so no other run
/// started in the same second can take it: the id's salt is only 16 bits,
/// and two runs that drew the same one shared a directory and the second
/// failed to create its journal.
pub fn reserve_id() -> String {
    reserve(Path::new(RUNS_DIR), new_id)
}

fn reserve(runs: &Path, mut next: impl FnMut() -> String) -> String {
    let _ = std::fs::create_dir_all(runs);
    let mut id = next();
    for _ in 0..1000 {
        match std::fs::create_dir(runs.join(&id)) {
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => id = next(),
            // Created, or an error the runtime will report with its own words.
            _ => break,
        }
    }
    id
}

/// Days since 1970-01-01 to a date (Howard Hinnant's algorithm).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

/// What the first line of a journal says about its run.
pub struct Header {
    pub program: PathBuf,
    pub graph: String,
    pub args: Value,
}

fn lines(id: &str) -> Result<Vec<Value>, String> {
    if calyx_runtime::pg::enabled() {
        return match calyx_runtime::pg::lines(id)? {
            Some(lines) => Ok(lines
                .iter()
                .filter_map(|l| serde_json::from_str(l).ok())
                .collect()),
            None => Err(format!(
                "no run `{id}` in the journal database; `calyx runs` lists the runs"
            )),
        };
    }
    let path = dir(id).join(JOURNAL);
    let text = std::fs::read_to_string(&path).map_err(|_| {
        format!(
            "no run `{id}` here (looked for {}); `calyx runs` lists the runs",
            path.display()
        )
    })?;
    // A torn last line (a crash during a write) is skipped.
    Ok(text
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect())
}

pub fn header(id: &str) -> Result<Header, String> {
    let lines = lines(id)?;
    let h = lines
        .first()
        .filter(|h| h["type"] == "run")
        .ok_or_else(|| format!("run `{id}` has no valid journal"))?;
    Ok(Header {
        program: PathBuf::from(h["program"].as_str().unwrap_or_default()),
        graph: h["graph"].as_str().unwrap_or_default().to_owned(),
        args: h["args"].clone(),
    })
}

pub struct Summary {
    pub id: String,
    pub graph: String,
    pub status: String,
    pub calls: usize,
    pub resumes: usize,
}

/// All runs, oldest first.
pub fn list() -> Vec<Summary> {
    let mut ids: Vec<String> = if calyx_runtime::pg::enabled() {
        match calyx_runtime::pg::runs() {
            Ok(runs) => runs.into_iter().map(|(id, _)| id).collect(),
            Err(e) => {
                eprintln!("calyx: journal database: {e}");
                Vec::new()
            }
        }
    } else {
        let Ok(entries) = std::fs::read_dir(RUNS_DIR) else {
            return Vec::new();
        };
        entries
            .filter_map(Result::ok)
            .filter(|e| e.path().join(JOURNAL).is_file())
            .filter_map(|e| e.file_name().into_string().ok())
            .collect()
    };
    ids.sort();
    ids.into_iter()
        .map(|id| {
            let lines = lines(&id).unwrap_or_default();
            let count = |t: &str| lines.iter().filter(|l| l["type"] == t).count();
            let status = match lines.last() {
                Some(l) if l["type"] == "end" && l.get("ok").is_some() => "finished".to_owned(),
                Some(l)
                    if l["type"] == "end"
                        && l["error"].as_str().is_some_and(|e| e.starts_with(WAITING)) =>
                {
                    "waiting".to_owned()
                }
                Some(l) if l["type"] == "end" => "failed".to_owned(),
                // With the journal in PostgreSQL, a run with no end may be
                // running right now, on any machine.
                _ if calyx_runtime::pg::enabled()
                    && calyx_runtime::pg::run_is_held(&id).unwrap_or(false) =>
                {
                    "running".to_owned()
                }
                _ => "interrupted".to_owned(),
            };
            Summary {
                graph: lines
                    .first()
                    .and_then(|h| h["graph"].as_str())
                    .unwrap_or("?")
                    .to_owned(),
                id,
                status,
                calls: count("call"),
                resumes: count("resume"),
            }
        })
        .collect()
}

/// A `receive` the run reached and that has no value yet.
pub struct Wait {
    pub key: String,
    pub message: String,
    /// Deadline, in seconds since 1970 (UTC).
    pub until: f64,
    /// A message was delivered to it and the run has not taken it yet.
    pub delivered: bool,
    /// What the message is about (`receive M about value`), if anything.
    pub about: Option<Value>,
}

fn jsonl(path: &Path) -> Vec<Value> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

/// The `receive`s of a run still waiting, oldest first.
pub fn waits(id: &str) -> Vec<Wait> {
    let d = dir(id);
    let done: std::collections::HashSet<String> = lines(id)
        .unwrap_or_default()
        .iter()
        .filter(|l| l["type"] == "call")
        .filter_map(|l| l["key"].as_str().map(str::to_owned))
        .collect();
    let inbox: std::collections::HashSet<String> = jsonl(&d.join("inbox.jsonl"))
        .iter()
        .filter_map(|l| l["key"].as_str().map(str::to_owned))
        .collect();
    jsonl(&d.join("waits.jsonl"))
        .into_iter()
        .filter_map(|w| {
            let key = w["key"].as_str()?.to_owned();
            if done.contains(&key) {
                return None;
            }
            Some(Wait {
                delivered: inbox.contains(&key),
                message: w["message"].as_str().unwrap_or("?").to_owned(),
                until: w["until"].as_f64().unwrap_or(0.0),
                about: w.get("about").cloned(),
                key,
            })
        })
        .collect()
}

/// Delivers `value` to the oldest `receive` of `message` the run waits on.
/// Returns its key. A message that comes after the deadline is refused,
/// even if the run has not taken its `on timeout` yet: the deadline is when
/// the answer had to arrive, not when the run happens to be resumed. The
/// time of delivery goes with the message, and the runtime checks it too.
pub fn deliver(id: &str, message: &str, value: &Value) -> Result<String, String> {
    let Some(w) = waits(id)
        .into_iter()
        .find(|w| w.message == message && !w.delivered)
    else {
        return Err(format!(
            "run `{id}` is not waiting for `{message}` (`calyx runs` shows what runs wait for)"
        ));
    };
    let at = now();
    if w.until > 0.0 && at >= w.until {
        return Err(format!(
            "the deadline for `{message}` at `{}` passed at {}: the run continues with `on timeout` (`calyx resume {id}` or `calyx tick`)",
            w.key,
            utc(w.until)
        ));
    }
    let line = serde_json::json!({"key": w.key, "message": message, "value": value, "at": at})
        .to_string()
        + "\n";
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir(id).join("inbox.jsonl"))
        .map_err(|e| format!("cannot write the run's inbox: {e}"))?;
    f.write_all(line.as_bytes())
        .and_then(|()| f.sync_all())
        .map_err(|e| format!("cannot write the run's inbox: {e}"))?;
    Ok(w.key)
}

/// Seconds since 1970, now.
pub fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// `YYYY-MM-DD HH:MM UTC` for a time in seconds since 1970.
pub fn utc(t: f64) -> String {
    let secs = t as i64;
    let (y, m, d) = civil_from_days(secs.div_euclid(86_400));
    let s = secs.rem_euclid(86_400);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02} UTC",
        s / 3600,
        s / 60 % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_days_to_dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(20_362), (2025, 10, 1));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
    }

    #[test]
    fn a_reserved_id_is_never_given_twice() {
        let runs = std::env::temp_dir().join(format!("calyx-reserve-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&runs);
        // Two runs that draw the same salt: the second gets the next id.
        let mut drawn = ["same", "same", "other"].into_iter().map(String::from);
        let first = reserve(&runs, || drawn.next().unwrap());
        let second = reserve(&runs, || drawn.next().unwrap());
        assert_eq!((first.as_str(), second.as_str()), ("same", "other"));
        let _ = std::fs::remove_dir_all(&runs);
    }

    #[test]
    fn ids_have_a_fixed_shape() {
        let id = new_id();
        assert_eq!(id.len(), 20, "{id}");
        assert_eq!(&id[8..9], "-");
        assert_eq!(&id[15..16], "-");
    }
}
