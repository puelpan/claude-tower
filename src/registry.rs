//! Claude Code's session registry: `<claude>/sessions/<pid>.json` (§3.2).
//!
//! Only `<digits>.json` files are read. The `<pid>.<hash>.key` files next to
//! them are never opened (§13).

use std::collections::HashMap;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde_json::{Map, Value};

/// One registry file's fields (§3.2). Every field is optional, and a field
/// with an unexpected JSON type counts as missing, so a format change
/// empties a column instead of dropping the row (§10). Unknown fields are
/// ignored.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Entry {
    pub pid: Option<u32>,
    /// Clock ticks since boot; a string in 2.1.322, a number is accepted too.
    pub proc_start: Option<String>,
    pub session_id: Option<String>,
    pub cwd: Option<String>,
    pub kind: Option<String>,
    /// `<session name>:<window id>.<pane id>`, e.g. `shop-api:@3.%3`.
    pub tmux: Option<String>,
    pub name: Option<String>,
    pub name_source: Option<String>,
    pub status: Option<String>,
    pub waiting_for: Option<String>,
    pub status_updated_at: Option<i64>,
    pub started_at: Option<i64>,
    pub updated_at: Option<i64>,
    pub version: Option<String>,
}

/// One `<pid>.json` file and what came out of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryFile {
    pub path: PathBuf,
    /// From the file name.
    pub pid: u32,
    /// The parsed entry, or why it couldn't be read or parsed.
    pub entry: Result<Entry, String>,
}

/// Reads the registry folder, re-reading a file only when its mtime or size
/// changed since the last scan.
#[derive(Debug)]
pub struct Registry {
    dir: PathBuf,
    /// By file name. Holds only files seen in the last scan.
    cache: HashMap<String, Cached>,
}

#[derive(Debug)]
struct Cached {
    mtime: SystemTime,
    len: u64,
    entry: Result<Entry, String>,
}

impl Registry {
    /// `dir` is `<claude>/sessions`.
    pub fn new(dir: PathBuf) -> Registry {
        Registry {
            dir,
            cache: HashMap::new(),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// All `<digits>.json` files, sorted by pid. Files that vanish between
    /// listing and reading are skipped. Errs only when the folder itself
    /// can't be read.
    pub fn scan(&mut self) -> std::io::Result<Vec<RegistryFile>> {
        let mut listed = Vec::new();
        for dent in fs::read_dir(&self.dir)? {
            let Ok(dent) = dent else { continue };
            // Filter on the name alone: nothing else is touched (§13).
            let name = dent.file_name();
            let Some(name) = name.to_str() else { continue };
            if let Some(pid) = pid_of_name(name) {
                listed.push((pid, name.to_string()));
            }
        }
        listed.sort();
        Ok(self.read_listed(listed))
    }

    /// Stats and, if changed, reads each listed file. Replaces the cache, so
    /// files that are gone drop out of it.
    fn read_listed(&mut self, listed: Vec<(u32, String)>) -> Vec<RegistryFile> {
        let mut old = std::mem::take(&mut self.cache);
        let mut out = Vec::with_capacity(listed.len());
        for (pid, name) in listed {
            let path = self.dir.join(&name);
            let meta = match fs::metadata(&path) {
                Ok(m) => m,
                Err(e) if e.kind() == ErrorKind::NotFound => continue,
                Err(e) => {
                    out.push(RegistryFile {
                        path,
                        pid,
                        entry: Err(e.to_string()),
                    });
                    continue;
                }
            };
            if !meta.is_file() {
                continue;
            }
            let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            let len = meta.len();
            let cached = old
                .remove(&name)
                .filter(|c| c.mtime == mtime && c.len == len);
            let entry = match cached {
                Some(c) => c.entry,
                None => match fs::read(&path) {
                    Ok(bytes) => parse_entry(&bytes),
                    Err(e) if e.kind() == ErrorKind::NotFound => continue,
                    // Not cached: a read error may clear without the file
                    // changing.
                    Err(e) => {
                        out.push(RegistryFile {
                            path,
                            pid,
                            entry: Err(e.to_string()),
                        });
                        continue;
                    }
                },
            };
            self.cache.insert(
                name,
                Cached {
                    mtime,
                    len,
                    entry: entry.clone(),
                },
            );
            out.push(RegistryFile { path, pid, entry });
        }
        out
    }
}

/// The pid in a registry file name: `^[0-9]+\.json$`, fitting in a u32.
fn pid_of_name(name: &str) -> Option<u32> {
    let digits = name.strip_suffix(".json")?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

/// Parses one registry file's bytes. Errs only on invalid JSON or a
/// non-object top level.
pub fn parse_entry(bytes: &[u8]) -> Result<Entry, String> {
    let value: Value = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    let Value::Object(m) = value else {
        return Err("not a JSON object".to_string());
    };
    Ok(Entry {
        pid: get_u32(&m, "pid"),
        proc_start: get_str_or_int(&m, "procStart"),
        session_id: get_str(&m, "sessionId"),
        cwd: get_str(&m, "cwd"),
        kind: get_str(&m, "kind"),
        tmux: get_str(&m, "tmux"),
        name: get_str(&m, "name"),
        name_source: get_str(&m, "nameSource"),
        status: get_str(&m, "status"),
        waiting_for: get_str(&m, "waitingFor"),
        status_updated_at: get_i64(&m, "statusUpdatedAt"),
        started_at: get_i64(&m, "startedAt"),
        updated_at: get_i64(&m, "updatedAt"),
        version: get_str(&m, "version"),
    })
}

pub(crate) fn get_str(m: &Map<String, Value>, key: &str) -> Option<String> {
    m.get(key)?.as_str().map(str::to_string)
}

pub(crate) fn get_u32(m: &Map<String, Value>, key: &str) -> Option<u32> {
    m.get(key)?.as_u64()?.try_into().ok()
}

fn get_i64(m: &Map<String, Value>, key: &str) -> Option<i64> {
    m.get(key)?.as_i64()
}

/// A string as is, or an integer in its decimal form.
pub(crate) fn get_str_or_int(m: &Map<String, Value>, key: &str) -> Option<String> {
    match m.get(key)? {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) if n.is_u64() || n.is_i64() => Some(n.to_string()),
        _ => None,
    }
}

/// The pane id at the end of the registry `tmux` field: `"shop-api:@3.%3"`
/// → `Some("%3")`. Session names can contain `:` and `.`, so split from
/// the right.
pub fn pane_id(tmux_field: &str) -> Option<&str> {
    let (_, pane) = tmux_field.rsplit_once('.')?;
    let digits = pane.strip_prefix('%')?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(pane)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{File, Permissions};
    use std::os::unix::fs::PermissionsExt;

    const FIXTURES: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/2.1.322/registry"
    );

    fn parse(json: &str) -> Entry {
        parse_entry(json.as_bytes()).unwrap()
    }

    #[test]
    fn real_shape() {
        let bytes = fs::read(format!("{FIXTURES}/1228510.json")).unwrap();
        let e = parse_entry(&bytes).unwrap();
        assert_eq!(
            e,
            Entry {
                pid: Some(1228510),
                proc_start: Some("13982212".into()),
                session_id: Some("1f4e2d3c-5b6a-4c7d-8e9f-0a1b2c3d4e5f".into()),
                cwd: Some("/home/me/projects/shop-api".into()),
                kind: Some("interactive".into()),
                tmux: Some("shop-api:@3.%3".into()),
                name: Some("shop-api-38".into()),
                name_source: Some("derived".into()),
                status: Some("idle".into()),
                waiting_for: None,
                status_updated_at: Some(1794415203184),
                started_at: Some(1794368660331),
                updated_at: Some(1794415203184),
                version: Some("2.1.322".into()),
            }
        );
    }

    #[test]
    fn every_status() {
        for s in ["busy", "waiting", "idle", "shell", "compacting"] {
            let e = parse(&format!(r#"{{"status":"{s}"}}"#));
            assert_eq!(e.status.as_deref(), Some(s));
            assert_eq!(e.waiting_for, None);
        }
    }

    #[test]
    fn every_waiting_for() {
        for w in [
            "permission prompt",
            "input needed",
            "goal proposal",
            "sandbox request",
            "worker request",
            "dialog open",
            "something new",
        ] {
            let e = parse(&format!(r#"{{"status":"waiting","waitingFor":"{w}"}}"#));
            assert_eq!(e.status.as_deref(), Some("waiting"));
            assert_eq!(e.waiting_for.as_deref(), Some(w));
        }
    }

    #[test]
    fn missing_fields() {
        assert_eq!(parse("{}"), Entry::default());
        let e = parse(r#"{"pid":7,"status":"busy"}"#);
        assert_eq!(e.pid, Some(7));
        assert_eq!(e.status.as_deref(), Some("busy"));
        assert_eq!(e.tmux, None);
        assert_eq!(e.proc_start, None);
    }

    #[test]
    fn unknown_fields_ignored() {
        let e = parse(
            r#"{"pid":7,"status":"idle","peerFeatures":["a"],"nested":{"x":1},
                "newThing":null,"asksForConvertedSkills":false}"#,
        );
        assert_eq!(e.pid, Some(7));
        assert_eq!(e.status.as_deref(), Some("idle"));
    }

    #[test]
    fn wrong_types_are_none() {
        let e = parse(
            r#"{"pid":"7","sessionId":12,"cwd":["/"],"kind":true,"tmux":null,
                "name":{},"nameSource":1,"status":2,"waitingFor":false,
                "statusUpdatedAt":"1","startedAt":1.5,"updatedAt":[],
                "version":2.1,"procStart":true}"#,
        );
        assert_eq!(e, Entry::default());
    }

    #[test]
    fn pid_out_of_range_is_none() {
        assert_eq!(parse(r#"{"pid":-1}"#).pid, None);
        assert_eq!(parse(r#"{"pid":4294967296}"#).pid, None);
        assert_eq!(parse(r#"{"pid":4294967295}"#).pid, Some(u32::MAX));
    }

    #[test]
    fn proc_start_string_or_number() {
        assert_eq!(
            parse(r#"{"procStart":"13982212"}"#).proc_start.as_deref(),
            Some("13982212")
        );
        assert_eq!(
            parse(r#"{"procStart":13982212}"#).proc_start.as_deref(),
            Some("13982212")
        );
        assert_eq!(parse(r#"{"procStart":1.5}"#).proc_start, None);
    }

    #[test]
    fn invalid_json() {
        assert!(parse_entry(b"").is_err());
        assert!(parse_entry(b"{\"pid\":").is_err());
        assert!(parse_entry(b"not json").is_err());
        assert!(parse_entry(b"\xff\xfe").is_err());
    }

    #[test]
    fn non_object_top_level() {
        for s in ["[]", "null", "1", "\"x\"", "true"] {
            assert!(parse_entry(s.as_bytes()).is_err(), "{s}");
        }
    }

    #[test]
    fn pane_ids() {
        assert_eq!(pane_id("shop-api:@3.%3"), Some("%3"));
        assert_eq!(pane_id("0:@14.%14"), Some("%14"));
        // Session names with `:` and `.`.
        assert_eq!(pane_id("a.b:c:@1.%2"), Some("%2"));
        assert_eq!(pane_id("v1.2:@5.%10"), Some("%10"));
        assert_eq!(pane_id("x.%9:@1.%7"), Some("%7"));
        assert_eq!(pane_id(""), None);
        assert_eq!(pane_id("%3"), None);
        assert_eq!(pane_id("shop-api:@3"), None);
        assert_eq!(pane_id("shop-api:@3."), None);
        assert_eq!(pane_id("shop-api:@3.3"), None);
        assert_eq!(pane_id("shop-api:@3.%"), None);
        assert_eq!(pane_id("shop-api:@3.%3x"), None);
        assert_eq!(pane_id("shop-api:@3.%3 "), None);
    }

    #[test]
    fn file_names() {
        assert_eq!(pid_of_name("1228510.json"), Some(1228510));
        assert_eq!(pid_of_name("0.json"), Some(0));
        for bad in [
            ".json",
            "abc.json",
            "12a.json",
            "-1.json",
            "+1.json",
            "1.5.json",
            "12.JSON",
            "12.json.tmp",
            "12.json~",
            "12",
            "1228510.0a2419d6.key",
            "99999999999.json",
        ] {
            assert_eq!(pid_of_name(bad), None, "{bad}");
        }
    }

    fn write(dir: &Path, name: &str, body: &str) {
        fs::write(dir.join(name), body).unwrap();
    }

    #[test]
    fn scan_mixed_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path();
        write(d, "300.json", r#"{"pid":300,"status":"busy"}"#);
        write(d, "20.json", "{broken");
        write(d, "notes.json", r#"{"pid":1}"#);
        write(d, "300.json.tmp", r#"{"pid":1}"#);
        fs::create_dir(d.join("40.json")).unwrap();
        let key = d.join("300.0a2419d604ffd38f.key");
        write(d, "300.0a2419d604ffd38f.key", "secret");
        fs::set_permissions(&key, Permissions::from_mode(0o000)).unwrap();

        let mut reg = Registry::new(d.to_path_buf());
        let files = reg.scan().unwrap();
        let pids: Vec<u32> = files.iter().map(|f| f.pid).collect();
        assert_eq!(pids, [20, 300]);
        assert!(files[0].entry.is_err());
        assert_eq!(files[0].path, d.join("20.json"));
        let e = files[1].entry.as_ref().unwrap();
        assert_eq!(e.status.as_deref(), Some("busy"));

        fs::set_permissions(&key, Permissions::from_mode(0o600)).unwrap();
    }

    #[test]
    fn scan_drops_deleted_files() {
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path();
        write(d, "1.json", r#"{"status":"idle"}"#);
        write(d, "2.json", r#"{"status":"busy"}"#);
        let mut reg = Registry::new(d.to_path_buf());
        assert_eq!(reg.scan().unwrap().len(), 2);
        assert_eq!(reg.cache.len(), 2);

        fs::remove_file(d.join("1.json")).unwrap();
        let files = reg.scan().unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].pid, 2);
        assert_eq!(reg.cache.len(), 1);
        assert!(!reg.cache.contains_key("1.json"));
    }

    #[test]
    fn vanished_after_listing_is_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path();
        write(d, "2.json", r#"{"status":"busy"}"#);
        let mut reg = Registry::new(d.to_path_buf());
        let files = reg.read_listed(vec![(1, "1.json".into()), (2, "2.json".into())]);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].pid, 2);
    }

    #[test]
    fn scan_uses_cache_until_mtime_or_len_changes() {
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path();
        let path = d.join("5.json");
        write(d, "5.json", r#"{"status":"busy"}"#);
        let mtime = fs::metadata(&path).unwrap().modified().unwrap();
        let mut reg = Registry::new(d.to_path_buf());
        let status = |reg: &mut Registry| {
            let files = reg.scan().unwrap();
            files[0].entry.as_ref().unwrap().status.clone().unwrap()
        };
        assert_eq!(status(&mut reg), "busy");

        // Same length and mtime: the cached entry is kept.
        write(d, "5.json", r#"{"status":"idle"}"#);
        File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(mtime)
            .unwrap();
        assert_eq!(status(&mut reg), "busy");

        // New mtime: re-read.
        let later = mtime + std::time::Duration::from_secs(1);
        File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(later)
            .unwrap();
        assert_eq!(status(&mut reg), "idle");

        // New length, same mtime: re-read.
        write(d, "5.json", r#"{"status":"shell"} "#);
        File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(later)
            .unwrap();
        assert_eq!(status(&mut reg), "shell");
    }

    #[test]
    fn scan_missing_folder_errs() {
        let tmp = tempfile::tempdir().unwrap();
        let mut reg = Registry::new(tmp.path().join("nope"));
        assert!(reg.scan().is_err());
    }

    #[test]
    fn scan_fixtures() {
        let mut reg = Registry::new(PathBuf::from(FIXTURES));
        let files = reg.scan().unwrap();
        let got: Vec<(u32, Option<String>, Option<String>)> = files
            .into_iter()
            .map(|f| {
                let e = f.entry.unwrap();
                assert_eq!(e.pid, Some(f.pid));
                assert_eq!(e.version.as_deref(), Some("2.1.322"));
                (f.pid, e.status, e.waiting_for)
            })
            .collect();
        assert_eq!(
            got,
            [
                (1228510, Some("idle".into()), None),
                (1230001, Some("busy".into()), None),
                (1230002, Some("shell".into()), None),
                (
                    1230003,
                    Some("waiting".into()),
                    Some("permission prompt".into())
                ),
            ]
        );
    }
}
