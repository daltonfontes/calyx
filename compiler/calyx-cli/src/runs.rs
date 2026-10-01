//! Run directories: `.calyx/runs/<id>/` under the current directory, each
//! with the journal the runtime writes (decisions D6, D14).

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

pub const RUNS_DIR: &str = ".calyx/runs";
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
    let Ok(entries) = std::fs::read_dir(RUNS_DIR) else {
        return Vec::new();
    };
    let mut ids: Vec<String> = entries
        .filter_map(Result::ok)
        .filter(|e| e.path().join(JOURNAL).is_file())
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    ids.sort();
    ids.into_iter()
        .map(|id| {
            let lines = lines(&id).unwrap_or_default();
            let count = |t: &str| lines.iter().filter(|l| l["type"] == t).count();
            let status = match lines.last() {
                Some(l) if l["type"] == "end" && l.get("ok").is_some() => "finished".to_owned(),
                Some(l) if l["type"] == "end" => "failed".to_owned(),
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
    fn ids_have_a_fixed_shape() {
        let id = new_id();
        assert_eq!(id.len(), 20, "{id}");
        assert_eq!(&id[8..9], "-");
        assert_eq!(&id[15..16], "-");
    }
}
