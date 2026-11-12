//! Session transcripts: `<claude>/projects/<slug>/<sessionId>.jsonl` (§3.3).
//!
//! A `Reader` reads a transcript once in full, then only the bytes appended
//! since, and keeps a `Digest` of the few records the dashboard shows.

use std::fs::{self, File};
use std::io;
use std::iter::Peekable;
use std::os::unix::fs::{FileExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::str::Chars;
use std::sync::OnceLock;

use memchr::memmem::{Finder, FinderRev};
use serde_json::Value;

use crate::model::PrLink;

/// Bytes read per call.
const BLOCK: usize = 64 * 1024;
/// Length caps, in chars.
const TITLE_MAX: usize = 200;
const TEXT_MAX: usize = 2000;
/// Claude cuts longer slugs and appends a hash.
const SLUG_MAX: usize = 200;

/// What the dashboard shows from a transcript (§3.3). For each field, the
/// last record in the file wins. Strings have control characters removed
/// and are capped in length; transcript text is never logged (§13).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Digest {
    /// `custom-title` → `customTitle`.
    pub custom_title: Option<String>,
    /// `ai-title` → `aiTitle`.
    pub ai_title: Option<String>,
    /// `system`/`away_summary` → `content`.
    pub recap: Option<String>,
    /// `last-prompt` → `lastPrompt`.
    pub last_prompt: Option<String>,
    /// Last `text` block of the last `assistant` record that has one.
    pub last_said: Option<String>,
    /// `pr-link` → `prNumber`, `prUrl`, `prRepository`.
    pub pr: Option<PrLink>,
    /// `permission-mode` → `permissionMode`.
    pub permission_mode: Option<String>,
    /// `gitBranch` of the last record that has one.
    pub git_branch: Option<String>,
    /// Newest `timestamp` seen, ms since epoch. Records within a turn are
    /// written a few seconds out of order, so this keeps the maximum.
    pub last_activity_ms: Option<i64>,
}

/// Incremental reader for one transcript file.
#[derive(Debug)]
pub struct Reader {
    path: PathBuf,
    /// Bytes read so far, `partial` included.
    offset: u64,
    /// (dev, ino) of the file read so far.
    id: Option<(u64, u64)>,
    /// Start of a line whose newline hasn't arrived yet.
    partial: Vec<u8>,
    digest: Digest,
    scanned: bool,
}

impl Reader {
    pub fn new(path: PathBuf) -> Reader {
        Reader {
            path,
            offset: 0,
            id: None,
            partial: Vec::new(),
            digest: Digest::default(),
            scanned: false,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Reads what was appended since the last call; the first call reads
    /// the whole file. If the file shrank or its inode changed, starts over
    /// from an empty digest. An incomplete last line is kept until its
    /// newline arrives. Returns whether the digest changed.
    pub fn update(&mut self) -> io::Result<bool> {
        let file = File::open(&self.path)?;
        let meta = file.metadata()?;
        let id = (meta.dev(), meta.ino());
        if self.id == Some(id) && meta.len() == self.offset {
            self.scanned = true;
            return Ok(false);
        }
        let before = self.digest.clone();
        if self.id != Some(id) || meta.len() < self.offset {
            self.offset = 0;
            self.partial.clear();
            self.digest = Digest::default();
        }
        self.id = Some(id);
        self.read_appended(&file)?;
        self.scanned = true;
        Ok(self.digest != before)
    }

    pub fn digest(&self) -> &Digest {
        &self.digest
    }

    /// True once the first full read has completed.
    pub fn is_scanned(&self) -> bool {
        self.scanned
    }

    /// Reads from `offset` to EOF and applies each complete line.
    fn read_appended(&mut self, file: &File) -> io::Result<()> {
        let mut block = vec![0; BLOCK];
        let mut partial = std::mem::take(&mut self.partial);
        let mut line_start = self.offset - partial.len() as u64;
        // (offset, length) of lines that may hold assistant text.
        let mut texts = Vec::new();
        let result = loop {
            let n = match file.read_at(&mut block, self.offset) {
                Ok(0) => break Ok(()),
                Ok(n) => n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => break Err(e),
            };
            let base = self.offset;
            self.offset += n as u64;
            let data = &block[..n];
            let mut start = 0;
            for end in memchr::memchr_iter(b'\n', data) {
                let line = if partial.is_empty() {
                    &data[start..end]
                } else {
                    partial.extend_from_slice(&data[start..end]);
                    &partial[..]
                };
                if scan_line(line, &mut self.digest) {
                    texts.push((line_start, line.len()));
                }
                partial.clear();
                start = end + 1;
                line_start = base + start as u64;
            }
            partial.extend_from_slice(&data[start..]);
        };
        partial.shrink_to(BLOCK);
        self.partial = partial;

        // Parse only the newest candidate, or older ones if it has no text.
        let mut line = Vec::new();
        for &(at, len) in texts.iter().rev() {
            line.resize(len, 0);
            if file.read_exact_at(&mut line, at).is_err() {
                break;
            }
            if let Some(text) = assistant_text(&line) {
                self.digest.last_said = Some(text);
                break;
            }
        }
        result
    }
}

/// Byte patterns searched before any parsing.
struct Needles {
    sidechain: Finder<'static>,
    git_branch: FinderRev<'static>,
    timestamp: FinderRev<'static>,
    /// Records parsed in full when found.
    records: [Finder<'static>; 6],
    assistant: FinderRev<'static>,
    text: Finder<'static>,
}

fn needles() -> &'static Needles {
    static NEEDLES: OnceLock<Needles> = OnceLock::new();
    NEEDLES.get_or_init(|| Needles {
        sidechain: Finder::new(b"\"isSidechain\":true"),
        git_branch: FinderRev::new(b"\"gitBranch\":\""),
        timestamp: FinderRev::new(b"\"timestamp\":\""),
        records: [
            Finder::new(b"\"type\":\"ai-title\""),
            Finder::new(b"\"type\":\"custom-title\""),
            Finder::new(b"\"type\":\"last-prompt\""),
            Finder::new(b"\"type\":\"pr-link\""),
            Finder::new(b"\"type\":\"permission-mode\""),
            Finder::new(b"\"subtype\":\"away_summary\""),
        ],
        assistant: FinderRev::new(b"\"type\":\"assistant\""),
        text: Finder::new(b"\"type\":\"text\""),
    })
}

/// Applies one line to `digest`. Returns true if the line may hold
/// assistant text; the caller parses only the newest such line.
fn scan_line(line: &[u8], digest: &mut Digest) -> bool {
    let n = needles();
    if line.is_empty() || n.sidechain.find(line).is_some() {
        return false;
    }
    if let Some(branch) = last_string(line, &n.git_branch) {
        set(&mut digest.git_branch, &branch, TEXT_MAX, false);
    }
    if let Some(ms) = last_string(line, &n.timestamp).and_then(|s| parse_timestamp_ms(&s)) {
        digest.last_activity_ms = Some(digest.last_activity_ms.map_or(ms, |old| old.max(ms)));
    }
    if n.records.iter().any(|f| f.find(line).is_some()) {
        apply_record(line, digest);
    }
    n.assistant.rfind(line).is_some() && n.text.find(line).is_some()
}

/// The JSON string after the last match of `needle`, which ends in `"`.
/// Claude writes the top-level key after any nested object, so the last
/// match is the record's own.
fn last_string(line: &[u8], needle: &FinderRev) -> Option<String> {
    let open = needle.rfind(line)? + needle.needle().len() - 1;
    let mut i = open + 1;
    loop {
        match line.get(i)? {
            b'"' => break,
            b'\\' => i += 2,
            _ => i += 1,
        }
    }
    serde_json::from_slice(&line[open..=i]).ok()
}

/// Parses a line that matched a record needle and applies it if its
/// top-level `type` is the expected one.
fn apply_record(line: &[u8], d: &mut Digest) {
    let Ok(v) = serde_json::from_slice::<Value>(line) else {
        return;
    };
    let field = |key: &str| v.get(key).and_then(Value::as_str).unwrap_or("");
    match field("type") {
        "custom-title" => set(&mut d.custom_title, field("customTitle"), TITLE_MAX, false),
        "ai-title" => set(&mut d.ai_title, field("aiTitle"), TITLE_MAX, false),
        "last-prompt" => set(&mut d.last_prompt, field("lastPrompt"), TEXT_MAX, true),
        "permission-mode" => set(
            &mut d.permission_mode,
            field("permissionMode"),
            TEXT_MAX,
            false,
        ),
        "system" if field("subtype") == "away_summary" => {
            set(&mut d.recap, field("content"), TEXT_MAX, true)
        }
        "pr-link" => {
            if let Some(number) = v.get("prNumber").and_then(Value::as_u64) {
                d.pr = Some(PrLink {
                    number,
                    url: clean(field("prUrl"), TEXT_MAX, false).unwrap_or_default(),
                    repository: clean(field("prRepository"), TEXT_MAX, false).unwrap_or_default(),
                });
            }
        }
        _ => {}
    }
}

/// The last `text` block of an `assistant` record, unless blank.
fn assistant_text(line: &[u8]) -> Option<String> {
    let v: Value = serde_json::from_slice(line).ok()?;
    if v.get("type")?.as_str()? != "assistant" {
        return None;
    }
    let blocks = v.get("message")?.get("content")?.as_array()?;
    let text = blocks
        .iter()
        .rev()
        .find(|b| b.get("type").and_then(Value::as_str) == Some("text"))?
        .get("text")?
        .as_str()?;
    clean(text, TEXT_MAX, true)
}

/// Stores `value` cleaned; a missing or blank value keeps the old one.
fn set(field: &mut Option<String>, value: &str, max: usize, multiline: bool) {
    if let Some(value) = clean(value, max, multiline) {
        *field = Some(value);
    }
}

/// `s` without escape sequences or control characters, tabs as spaces,
/// trimmed and cut to `max` chars. Keeps newlines if `multiline`, else
/// turns them into spaces. `None` if nothing is left.
fn clean(s: &str, max: usize, multiline: bool) -> Option<String> {
    let mut out = String::with_capacity(s.len().min(max));
    let mut count = 0;
    let mut chars = s.chars().peekable();
    while count < max {
        let Some(c) = chars.next() else { break };
        let c = match c {
            '\u{1b}' => {
                skip_escape(&mut chars);
                continue;
            }
            '\n' if multiline => '\n',
            '\t' | '\n' => ' ',
            c if c.is_control() => continue,
            c => c,
        };
        if out.is_empty() && c.is_whitespace() {
            continue;
        }
        out.push(c);
        count += 1;
    }
    out.truncate(out.trim_end().len());
    (!out.is_empty()).then_some(out)
}

/// Skips the rest of an escape sequence after ESC: CSI (`[` … final byte),
/// OSC and other string commands (… BEL or ST), or a short `ESC x`.
fn skip_escape(chars: &mut Peekable<Chars>) {
    match chars.next() {
        Some('[') => {
            // Parameter and intermediate bytes, then one final byte.
            for c in chars.by_ref() {
                if !('\u{20}'..='\u{3f}').contains(&c) {
                    break;
                }
            }
        }
        Some(']' | 'P' | 'X' | '^' | '_') => {
            while let Some(c) = chars.next() {
                if c == '\u{7}' || c == '\u{9c}' {
                    break;
                }
                if c == '\u{1b}' {
                    chars.next_if_eq(&'\\');
                    break;
                }
            }
        }
        Some('\u{20}'..='\u{2f}') => {
            // Intermediate bytes, then one final byte.
            for c in chars.by_ref() {
                if !('\u{20}'..='\u{2f}').contains(&c) {
                    break;
                }
            }
        }
        _ => {}
    }
}

/// Claude's folder name for a cwd: every UTF-16 unit other than an ASCII
/// letter or digit becomes `-`. Past 200 chars Claude keeps the first 200
/// and appends `-` and a hash of the cwd.
pub fn slug(cwd: &str) -> String {
    let slug: String = cwd
        .encode_utf16()
        .map(|unit| match u8::try_from(unit) {
            Ok(b) if b.is_ascii_alphanumeric() => char::from(b),
            _ => '-',
        })
        .collect();
    if slug.len() <= SLUG_MAX {
        return slug;
    }
    // Java's String.hashCode over UTF-16 units, printed in base 36.
    let hash = cwd.encode_utf16().fold(0i32, |h, unit| {
        h.wrapping_mul(31).wrapping_add(i32::from(unit))
    });
    format!("{}-{}", &slug[..SLUG_MAX], base36(hash.unsigned_abs()))
}

fn base36(mut n: u32) -> String {
    let mut digits = Vec::new();
    loop {
        digits.extend(char::from_digit(n % 36, 36));
        n /= 36;
        if n == 0 {
            break;
        }
    }
    digits.iter().rev().collect()
}

/// The transcript for a session: `<claude>/projects/<slug(cwd)>/<id>.jsonl`,
/// else the most recently modified `<claude>/projects/*/<id>.jsonl`. `None`
/// if neither exists or `session_id` isn't a safe file name (letters,
/// digits, `-`).
pub fn locate(claude_dir: &Path, cwd: &str, session_id: &str) -> Option<PathBuf> {
    let safe = !session_id.is_empty()
        && session_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-');
    if !safe {
        return None;
    }
    let name = format!("{session_id}.jsonl");
    let projects = claude_dir.join("projects");
    let direct = projects.join(slug(cwd)).join(&name);
    if direct.is_file() {
        return Some(direct);
    }
    fs::read_dir(&projects)
        .ok()?
        .flatten()
        .filter_map(|entry| {
            let path = entry.path().join(&name);
            let meta = fs::metadata(&path).ok()?;
            meta.is_file().then(|| (meta.modified().ok(), path))
        })
        .max_by_key(|(modified, _)| *modified)
        .map(|(_, path)| path)
}

/// Parses an RFC 3339 UTC timestamp such as `2026-11-11T16:56:59.538Z`
/// (also `+00:00` offsets and no fraction) to ms since epoch. Other
/// offsets are applied; digits past milliseconds are dropped.
pub fn parse_timestamp_ms(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    let num = |from: usize, to: usize| -> Option<i64> {
        let digits = b.get(from..to)?;
        digits.iter().try_fold(0i64, |n, &d| {
            d.is_ascii_digit().then(|| n * 10 + i64::from(d - b'0'))
        })
    };
    let sep = |at: usize, ok: &[u8]| b.get(at).is_some_and(|c| ok.contains(c));
    if !(sep(4, b"-") && sep(7, b"-") && sep(10, b"Tt ") && sep(13, b":") && sep(16, b":")) {
        return None;
    }
    let (year, month, day) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (hour, minute, second) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);

    let mut i = 19;
    let mut ms = 0;
    if b.get(i) == Some(&b'.') {
        i += 1;
        let start = i;
        while b.get(i).is_some_and(u8::is_ascii_digit) {
            if i - start < 3 {
                ms = ms * 10 + i64::from(b[i] - b'0');
            }
            i += 1;
        }
        match i - start {
            0 => return None,
            1 => ms *= 100,
            2 => ms *= 10,
            _ => {}
        }
    }
    let offset_min = match b.get(i..)? {
        b"Z" | b"z" => 0,
        [sign @ (b'+' | b'-'), _, _, b':', _, _] => {
            let (h, m) = (num(i + 1, i + 3)?, num(i + 4, i + 6)?);
            if h > 23 || m > 59 {
                return None;
            }
            if *sign == b'+' {
                h * 60 + m
            } else {
                -(h * 60 + m)
            }
        }
        _ => return None,
    };

    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let month_days = match month {
        2 if leap => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        1..=12 => 31,
        _ => return None,
    };
    if day < 1 || day > month_days || hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    let secs = days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second
        - offset_min * 60;
    Some(secs * 1000 + ms)
}

/// Days since 1970-01-01 for a proleptic Gregorian date (H. Hinnant).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::time::{Duration, SystemTime};

    use serde_json::json;

    use super::*;

    const TS: &str = "2026-11-11T16:56:59.538Z";
    const TS_MS: i64 = 1_794_416_219_538;

    fn line(v: Value) -> String {
        format!("{v}\n")
    }

    /// A message record in Claude's key order: record fields first, then
    /// `timestamp`, ids, and `gitBranch` last.
    fn record(fields: &str, timestamp: &str, branch: &str) -> String {
        format!(
            r#"{{"parentUuid":"p","isSidechain":false,{fields},"timestamp":"{timestamp}","uuid":"u","cwd":"/w","sessionId":"s","version":"2.1.322","gitBranch":"{branch}"}}"#
        ) + "\n"
    }

    /// An assistant record: the nested `message` comes before the
    /// top-level `type`, as in real files.
    fn assistant(content: Value) -> String {
        let message = json!({"model": "m", "id": "msg_1", "type": "message", "role": "assistant", "content": content});
        record(
            &format!(r#""message":{message},"requestId":"req_1","type":"assistant""#),
            TS,
            "main",
        )
    }

    fn said(text: &str) -> String {
        assistant(json!([{"type": "text", "text": text}]))
    }

    fn tool_use(input: Value) -> String {
        assistant(json!([{"type": "tool_use", "id": "toolu_1", "name": "Bash", "input": input}]))
    }

    fn thinking() -> String {
        assistant(json!([{"type": "thinking", "thinking": "", "signature": "sig"}]))
    }

    fn user(content: &str, timestamp: &str, branch: &str) -> String {
        let message = json!({"role": "user", "content": content});
        record(
            &format!(r#""type":"user","message":{message}"#),
            timestamp,
            branch,
        )
    }

    fn recap(text: &str) -> String {
        let fields = format!(
            r#""type":"system","subtype":"away_summary","content":{}"#,
            json!(text)
        );
        record(&fields, "2026-11-11T16:56:04.388Z", "main")
    }

    fn title(text: &str) -> String {
        line(json!({"type": "ai-title", "aiTitle": text, "sessionId": "s"}))
    }

    fn custom_title(text: &str) -> String {
        line(json!({"type": "custom-title", "customTitle": text, "sessionId": "s"}))
    }

    fn prompt(text: &str) -> String {
        line(json!({"type": "last-prompt", "lastPrompt": text, "leafUuid": "u", "sessionId": "s"}))
    }

    fn pr(number: u64) -> String {
        line(json!({
            "type": "pr-link", "sessionId": "s", "prNumber": number,
            "prUrl": format!("https://github.com/o/r/pull/{number}"),
            "prRepository": "o/r", "timestamp": TS,
        }))
    }

    fn mode(mode: &str) -> String {
        line(json!({"type": "permission-mode", "permissionMode": mode, "sessionId": "s"}))
    }

    /// A reader on a temp file, with a handle to append to it.
    struct Fixture {
        _dir: tempfile::TempDir,
        path: PathBuf,
        file: File,
        reader: Reader,
    }

    impl Fixture {
        fn new() -> Fixture {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("t.jsonl");
            let file = File::create(&path).unwrap();
            let reader = Reader::new(path.clone());
            Fixture {
                _dir: dir,
                path,
                file,
                reader,
            }
        }

        /// Appends `bytes`, then updates; returns whether the digest changed.
        fn append(&mut self, bytes: impl AsRef<[u8]>) -> bool {
            self.file.write_all(bytes.as_ref()).unwrap();
            self.reader.update().unwrap()
        }

        fn digest(&self) -> &Digest {
            self.reader.digest()
        }
    }

    fn digest_of(text: impl AsRef<[u8]>) -> Digest {
        let mut f = Fixture::new();
        f.append(text);
        f.digest().clone()
    }

    #[test]
    fn each_record_type() {
        let text = [
            custom_title("Checkout redesign"),
            title("Issue 882"),
            recap("You're planning the fix.\nNext: tests."),
            prompt("rerun the checks"),
            said("All checks pass."),
            pr(891),
            mode("auto"),
        ]
        .concat();
        let want = Digest {
            custom_title: Some("Checkout redesign".into()),
            ai_title: Some("Issue 882".into()),
            recap: Some("You're planning the fix.\nNext: tests.".into()),
            last_prompt: Some("rerun the checks".into()),
            last_said: Some("All checks pass.".into()),
            pr: Some(PrLink {
                number: 891,
                url: "https://github.com/o/r/pull/891".into(),
                repository: "o/r".into(),
            }),
            permission_mode: Some("auto".into()),
            git_branch: Some("main".into()),
            last_activity_ms: Some(TS_MS),
        };
        assert_eq!(digest_of(text), want);
    }

    #[test]
    fn last_record_of_each_kind_wins() {
        let text = [
            custom_title("One"),
            title("One"),
            recap("One"),
            prompt("One"),
            said("One"),
            pr(1),
            mode("default"),
            user("hi", "2026-11-11T10:00:00.000Z", "main"),
            custom_title("Two"),
            title("Two"),
            recap("Two"),
            prompt("Two"),
            said("Two"),
            pr(2),
            mode("auto"),
            user("hi", "2026-11-11T11:00:00.000Z", "feat/two"),
        ]
        .concat();
        let d = digest_of(text);
        assert_eq!(d.custom_title.as_deref(), Some("Two"));
        assert_eq!(d.ai_title.as_deref(), Some("Two"));
        assert_eq!(d.recap.as_deref(), Some("Two"));
        assert_eq!(d.last_prompt.as_deref(), Some("Two"));
        assert_eq!(d.last_said.as_deref(), Some("Two"));
        assert_eq!(d.pr.map(|pr| pr.number), Some(2));
        assert_eq!(d.permission_mode.as_deref(), Some("auto"));
        assert_eq!(d.git_branch.as_deref(), Some("feat/two"));
    }

    #[test]
    fn last_prompt_record_without_text_keeps_the_old_one() {
        // Claude writes this shape when it has no prompt to record.
        let empty = line(json!({"type": "last-prompt", "leafUuid": "u", "sessionId": "s"}));
        let d = digest_of([prompt("Fix it"), empty].concat());
        assert_eq!(d.last_prompt.as_deref(), Some("Fix it"));
    }

    #[test]
    fn assistant_without_text_keeps_last_said() {
        let mut f = Fixture::new();
        f.append(
            [
                said("Here is the plan."),
                tool_use(json!({"command": "ls"})),
                thinking(),
            ]
            .concat(),
        );
        assert_eq!(f.digest().last_said.as_deref(), Some("Here is the plan."));

        f.append(
            [
                tool_use(json!({"command": "ls"})),
                said(" \n\t "),
                thinking(),
            ]
            .concat(),
        );
        assert_eq!(f.digest().last_said.as_deref(), Some("Here is the plan."));

        f.append(said("Done."));
        assert_eq!(f.digest().last_said.as_deref(), Some("Done."));
    }

    #[test]
    fn last_text_block_of_a_record() {
        let content = json!([
            {"type": "text", "text": "first"},
            {"type": "tool_use", "id": "t1", "name": "Read", "input": {}},
            {"type": "text", "text": "second"},
            {"type": "tool_use", "id": "t2", "name": "Read", "input": {}},
        ]);
        assert_eq!(
            digest_of(assistant(content)).last_said.as_deref(),
            Some("second")
        );
    }

    #[test]
    fn newest_candidate_without_text_falls_back_within_the_read() {
        // Both needles match, but the text block is nested in a tool input.
        let nested = tool_use(json!({"blocks": [{"type": "text", "text": "nested"}]}));
        let d = digest_of([said("Real text."), nested].concat());
        assert_eq!(d.last_said.as_deref(), Some("Real text."));
    }

    #[test]
    fn nested_and_escaped_needles_are_not_records() {
        let nested = tool_use(json!({"records": [
            {"type": "custom-title", "customTitle": "nested"},
            {"type": "ai-title", "aiTitle": "nested"},
            {"type": "last-prompt", "lastPrompt": "nested"},
            {"type": "pr-link", "prNumber": 1, "prUrl": "u", "prRepository": "r"},
            {"type": "permission-mode", "permissionMode": "nested"},
            {"type": "system", "subtype": "away_summary", "content": "nested"},
        ]}));
        let escaped = user(
            r#"{"type":"ai-title","aiTitle":"x"} {"type":"assistant","message":{"content":[{"type":"text","text":"x"}]}}"#,
            TS,
            "main",
        );
        let want = Digest {
            git_branch: Some("main".into()),
            last_activity_ms: Some(TS_MS),
            ..Digest::default()
        };
        assert_eq!(digest_of([nested, escaped].concat()), want);
    }

    #[test]
    fn branch_and_timestamp_come_from_the_last_key() {
        // A tool input's keys come before the record's own.
        let line =
            tool_use(json!({"gitBranch": "nested", "timestamp": "2030-01-01T00:00:00.000Z"}));
        let d = digest_of(line);
        assert_eq!(d.git_branch.as_deref(), Some("main"));
        assert_eq!(d.last_activity_ms, Some(TS_MS));
    }

    #[test]
    fn branch_escapes_and_bad_timestamps() {
        let d = digest_of(record(r#""type":"user""#, "not a time", r"feat\/xé"));
        assert_eq!(d.git_branch.as_deref(), Some("feat/xé"));
        assert_eq!(d.last_activity_ms, None);
    }

    #[test]
    fn last_activity_keeps_the_newest() {
        // Records within a turn can be stamped a little out of order.
        let text = [
            user("a", "2026-11-11T16:56:10.000Z", "main"),
            record(r#""type":"attachment""#, "2026-11-11T16:56:05.000Z", "main"),
        ]
        .concat();
        assert_eq!(digest_of(text).last_activity_ms, Some(1_794_416_170_000));
    }

    #[test]
    fn sidechain_records_are_skipped() {
        let side = |s: String| s.replace(r#""isSidechain":false"#, r#""isSidechain":true"#);
        let text = [
            said("Main."),
            side(said("Side.")),
            side(user("x", "2030-01-01T00:00:00.000Z", "side-branch")),
            side(recap("Side recap")),
        ]
        .concat();
        let d = digest_of(text);
        assert_eq!(d.last_said.as_deref(), Some("Main."));
        assert_eq!(d.git_branch.as_deref(), Some("main"));
        assert_eq!(d.last_activity_ms, Some(TS_MS));
        assert_eq!(d.recap, None);
    }

    #[test]
    fn partial_line_waits_for_its_newline() {
        let text = [title("Título ✳ ñ"), prompt("next")].concat();
        let bytes = text.as_bytes();
        let glyph = text.find('✳').unwrap();
        let newline = text.find('\n').unwrap();

        let mut f = Fixture::new();
        // Cut inside the 3-byte char.
        assert!(!f.append(&bytes[..glyph + 1]));
        assert!(f.reader.is_scanned());
        assert_eq!(f.digest(), &Digest::default());
        assert!(!f.append(&bytes[glyph + 1..newline]));
        assert!(f.append(&bytes[newline..newline + 1]));
        assert_eq!(f.digest().ai_title.as_deref(), Some("Título ✳ ñ"));
        assert_eq!(f.digest().last_prompt, None);
        assert!(f.append(&bytes[newline + 1..]));
        assert_eq!(f.digest().last_prompt.as_deref(), Some("next"));
    }

    #[test]
    fn byte_by_byte_matches_one_read() {
        let text = [
            title("Título ✳"),
            said("Olá — ok"),
            recap("Résumé"),
            tool_use(json!({"command": "ls"})),
        ]
        .concat();
        let mut f = Fixture::new();
        for &b in text.as_bytes() {
            f.append([b]);
        }
        assert_eq!(f.digest(), &digest_of(&text));
        assert_eq!(f.digest().last_said.as_deref(), Some("Olá — ok"));
    }

    #[test]
    fn shrunk_file_is_read_again() {
        let mut f = Fixture::new();
        f.append(
            [
                title("Old"),
                pr(7),
                prompt("old"),
                user(&"pad ".repeat(100), TS, "main"),
            ]
            .concat(),
        );
        assert!(f.digest().pr.is_some());

        // Same inode, shorter content.
        f.file = File::create(&f.path).unwrap();
        assert!(f.append(title("New")));
        let want = Digest {
            ai_title: Some("New".into()),
            ..Digest::default()
        };
        assert_eq!(f.digest(), &want);
    }

    #[test]
    fn replaced_file_is_read_again() {
        let mut f = Fixture::new();
        f.append([title("Old"), pr(7)].concat());
        let old_ino = fs::metadata(&f.path).unwrap().ino();

        // A longer file renamed over the old one: new inode, not shorter.
        let new = f.path.with_extension("new");
        fs::write(
            &new,
            [title("New"), user(&"x".repeat(500), TS, "main")].concat(),
        )
        .unwrap();
        fs::rename(&new, &f.path).unwrap();
        assert_ne!(fs::metadata(&f.path).unwrap().ino(), old_ino);

        assert!(f.reader.update().unwrap());
        let want = Digest {
            ai_title: Some("New".into()),
            git_branch: Some("main".into()),
            last_activity_ms: Some(TS_MS),
            ..Digest::default()
        };
        assert_eq!(f.digest(), &want);
    }

    #[test]
    fn missing_file_is_not_found_and_keeps_state() {
        let mut f = Fixture::new();
        f.append(title("Kept"));
        fs::remove_file(&f.path).unwrap();
        let err = f.reader.update().unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        assert_eq!(f.digest().ai_title.as_deref(), Some("Kept"));
        assert!(f.reader.is_scanned());

        let mut never = Reader::new(f.path.with_file_name("none.jsonl"));
        assert_eq!(never.update().unwrap_err().kind(), io::ErrorKind::NotFound);
        assert!(!never.is_scanned());
        assert_eq!(never.digest(), &Digest::default());
    }

    #[test]
    fn update_reports_changes() {
        let mut f = Fixture::new();
        assert!(!f.reader.is_scanned());
        assert!(!f.reader.update().unwrap());
        assert!(f.reader.is_scanned());
        assert!(f.append(title("A")));
        assert!(!f.reader.update().unwrap());
        assert!(!f.append(title("A")));
        assert!(!f.append(line(json!({"type": "mode", "mode": "normal"}))));
        assert!(f.append(title("B")));
    }

    #[test]
    fn malformed_lines_are_skipped() {
        let mut text = [title("Good"), said("Good text.")].concat().into_bytes();
        for bad in [
            "not json at all",
            r#"{"type":"ai-title","aiTitle":"Broken""#,
            r#"{"type":"ai-title","aiTitle":42}"#,
            r#"{"type":"ai-title"}"#,
            r#"[{"type":"ai-title","aiTitle":"In an array"}]"#,
            r#"{"type":"pr-link","prNumber":"891","prUrl":"u"}"#,
            r#"{"type":"assistant","type":"text""#,
            r#"{"message":{"content":[{"type":"text","text":"x"}]},"type":"assistant""#,
            "",
            "   ",
        ] {
            text.extend_from_slice(bad.as_bytes());
            text.push(b'\n');
        }
        text.extend_from_slice(b"{\"type\":\"ai-title\",\"aiTitle\":\"bad \xff utf-8\"}\n");
        let d = digest_of(&text);
        assert_eq!(d.ai_title.as_deref(), Some("Good"));
        assert_eq!(d.last_said.as_deref(), Some("Good text."));
        assert_eq!(d.pr, None);

        let crlf = digest_of("{\"type\":\"ai-title\",\"aiTitle\":\"CRLF\"}\r\n");
        assert_eq!(crlf.ai_title.as_deref(), Some("CRLF"));
    }

    #[test]
    fn line_longer_than_a_block() {
        let long = "x".repeat(3 * BLOCK) + "end";
        let text = [title("Before"), said(&long), title("After")].concat();
        let d = digest_of(&text);
        assert_eq!(d.ai_title.as_deref(), Some("After"));
        assert_eq!(d.last_said, Some("x".repeat(TEXT_MAX)));

        // Arriving in pieces smaller than the line.
        let mut f = Fixture::new();
        for piece in text.as_bytes().chunks(10_000) {
            f.append(piece);
        }
        assert_eq!(f.digest(), &d);
    }

    #[test]
    fn clean_strips_escapes_and_controls() {
        for (raw, want) in [
            ("\x1b[1;31mRed\x1b[0m plain", "Red plain"),
            ("\x1b]0;window title\x07Text", "Text"),
            ("\x1b]8;;https://x.invalid\x1b\\link\x1b]8;;\x1b\\", "link"),
            ("\x1b(Bcharset \x1b7saved", "charset saved"),
            ("a\tb", "a b"),
            ("one\ntwo", "one two"),
            ("bell\x07 nul\0 del\x7f c1\u{85} end", "bell nul del c1 end"),
            ("  padded \n ", "padded"),
            ("trailing escape\x1b", "trailing escape"),
            ("unfinished csi \x1b[12", "unfinished csi"),
        ] {
            assert_eq!(
                clean(raw, TEXT_MAX, false).as_deref(),
                Some(want),
                "{raw:?}"
            );
        }
        let multiline = clean("one\ntwo\r\n\tthree\n", TEXT_MAX, true);
        assert_eq!(multiline.as_deref(), Some("one\ntwo\n three"));
        for blank in ["", "   ", "\n\t", "\x1b[0m", "\0"] {
            assert_eq!(clean(blank, TEXT_MAX, true), None, "{blank:?}");
        }
    }

    #[test]
    fn stored_strings_are_sanitized() {
        let text = [
            title("\x1b[1mBold\x1b[0m\ttitle\nnext"),
            recap("Line one.\n\x1b]0;x\x07Line\ttwo."),
            prompt("\x1b[31mred\x1b[0m\nsecond line"),
        ]
        .concat();
        let d = digest_of(text);
        assert_eq!(d.ai_title.as_deref(), Some("Bold title next"));
        assert_eq!(d.recap.as_deref(), Some("Line one.\nLine two."));
        assert_eq!(d.last_prompt.as_deref(), Some("red\nsecond line"));
    }

    #[test]
    fn stored_strings_are_capped_on_char_boundaries() {
        let text = [
            title(&"é".repeat(300)),
            custom_title(&"界".repeat(201)),
            prompt(&"ü".repeat(3000)),
            said(&"✳".repeat(2500)),
        ]
        .concat();
        let d = digest_of(text);
        assert_eq!(d.ai_title, Some("é".repeat(TITLE_MAX)));
        assert_eq!(d.custom_title, Some("界".repeat(TITLE_MAX)));
        assert_eq!(d.last_prompt, Some("ü".repeat(TEXT_MAX)));
        assert_eq!(d.last_said, Some("✳".repeat(TEXT_MAX)));
    }

    #[test]
    fn sample_fixture() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/2.1.322/transcripts/sample.jsonl");
        let mut reader = Reader::new(path.clone());
        assert!(reader.update().unwrap());
        let want = Digest {
            custom_title: None,
            ai_title: Some("Placeholder title two".into()),
            recap: Some("Placeholder recap: the work is in PR 891.".into()),
            last_prompt: Some("Placeholder prompt two.".into()),
            last_said: Some("Placeholder answer three.\n\nSecond paragraph.".into()),
            pr: Some(PrLink {
                number: 891,
                url: "https://github.com/example-org/example-repo/pull/891".into(),
                repository: "example-org/example-repo".into(),
            }),
            permission_mode: Some("auto".into()),
            git_branch: Some("feat/placeholder".into()),
            last_activity_ms: Some(1_794_412_912_144),
        };
        assert_eq!(reader.digest(), &want);

        // The same bytes appended in pieces.
        let mut f = Fixture::new();
        for piece in fs::read(&path).unwrap().chunks(7_919) {
            f.append(piece);
        }
        assert_eq!(f.digest(), &want);
    }

    #[test]
    fn slug_matches_claude() {
        assert_eq!(
            slug("/home/me/projects/web-front/.claude/worktrees/feat-x"),
            "-home-me-projects-web-front--claude-worktrees-feat-x"
        );
        assert_eq!(slug("/home/me/shop_api v2"), "-home-me-shop-api-v2");
        // One dash per UTF-16 unit, so two for a char outside the BMP.
        assert_eq!(slug("/home/me/café/naïve 🚀x"), "-home-me-caf--na-ve---x");
        assert_eq!(slug(""), "");

        // Expected values from Claude's own function (Java's
        // String.hashCode over UTF-16 units, in base 36).
        let at_limit = format!("/home/me/{}", "a".repeat(191));
        assert_eq!(slug(&at_limit), format!("-home-me-{}", "a".repeat(191)));
        let over = format!("/home/me/{}", "a".repeat(192));
        assert_eq!(slug(&over), format!("-home-me-{}-178n6y", "a".repeat(191)));
        let deep = format!("/home/me/{}end", "deep/".repeat(40));
        assert_eq!(
            slug(&deep),
            format!("-home-me-{}d-9o4t1h", "deep-".repeat(38))
        );
        let long = format!("/srv/{}", "z".repeat(300));
        assert_eq!(slug(&long), format!("-srv-{}-o5kvrb", "z".repeat(195)));
    }

    /// Creates `<claude>/projects/<folder>/<name>`.
    fn touch(claude: &Path, folder: &str, name: &str) -> PathBuf {
        let dir = claude.join("projects").join(folder);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        fs::write(&path, "").unwrap();
        path
    }

    fn set_age(path: &Path, secs: u64) {
        let file = File::options().write(true).open(path).unwrap();
        file.set_modified(SystemTime::now() - Duration::from_secs(secs))
            .unwrap();
    }

    #[test]
    fn locate_slug_folder_then_any_folder() {
        let claude = tempfile::tempdir().unwrap();
        let claude = claude.path();
        let cwd = "/home/me/projects/shop-api";
        let id = "1f4e2d3c-5b6a-4c7d-8e9f-0a1b2c3d4e5f";
        let name = format!("{id}.jsonl");
        assert_eq!(locate(claude, cwd, id), None);

        // The cwd changed since the session started.
        let moved = touch(claude, "-home-me-old-place", &name);
        assert_eq!(locate(claude, cwd, id), Some(moved));

        let direct = touch(claude, "-home-me-projects-shop-api", &name);
        set_age(&direct, 3600);
        assert_eq!(locate(claude, cwd, id), Some(direct));

        assert_eq!(
            locate(claude, cwd, "00000000-0000-4000-8000-000000000000"),
            None
        );
    }

    #[test]
    fn locate_fallback_prefers_newest() {
        let claude = tempfile::tempdir().unwrap();
        let claude = claude.path();
        let a = touch(claude, "-a", "abc.jsonl");
        let b = touch(claude, "-b", "abc.jsonl");
        set_age(&a, 3600);
        assert_eq!(locate(claude, "/elsewhere", "abc"), Some(b.clone()));
        set_age(&b, 7200);
        assert_eq!(locate(claude, "/elsewhere", "abc"), Some(a));
    }

    #[test]
    fn locate_rejects_unsafe_ids() {
        let claude = tempfile::tempdir().unwrap();
        let claude = claude.path();
        touch(claude, "-w", "ok.jsonl");
        touch(claude, "", "evil.jsonl");
        assert!(locate(claude, "/w", "ok").is_some());
        for id in [
            "../evil", "..", "", "ok.jsonl", "-w/ok", "ok\0", "ok ", "ök", "*",
        ] {
            assert_eq!(locate(claude, "/w", id), None, "{id:?}");
        }
    }

    #[test]
    fn parse_timestamp_known_values() {
        for (s, ms) in [
            ("1970-01-01T00:00:00Z", 0),
            ("1970-01-01T00:00:00.001Z", 1),
            ("1969-12-31T23:59:59.999Z", -1),
            ("2000-01-01T00:00:00Z", 946_684_800_000),
            ("2024-02-29T12:00:00Z", 1_709_208_000_000),
            ("1900-03-01T00:00:00Z", -2_203_891_200_000),
            ("9999-12-31T23:59:59Z", 253_402_300_799_000),
            ("2026-11-11T16:56:04.388Z", 1_794_416_164_388),
            ("2026-11-11T16:56:59.538Z", TS_MS),
            ("2026-11-11T16:56:59.538+00:00", TS_MS),
            ("2026-11-11T16:56:59.538-00:00", TS_MS),
            ("2026-11-11T16:56:59.538123Z", TS_MS),
            ("2026-11-11T16:56:59Z", TS_MS - 538),
            ("2026-11-11T16:56:59.5Z", TS_MS - 38),
            ("2026-11-11T18:56:59.538+02:00", TS_MS),
            ("2026-11-11T14:26:59.538-02:30", TS_MS),
            ("2026-11-11t16:56:59.538z", TS_MS),
        ] {
            assert_eq!(parse_timestamp_ms(s), Some(ms), "{s}");
        }
        for bad in [
            "",
            "2026-11-11",
            "2026-11-11T16:56",
            "2026-11-11T16:56:59",
            "2026-11-11T16:56:59.538",
            "2026-11-11T16:56:59.Z",
            "2026-11-11T16:56:59.538+0000",
            "2026-11-11T16:56:59.538+24:00",
            "2026-11-11T16:56:59.538Zjunk",
            "2026-13-01T00:00:00Z",
            "2026-00-01T00:00:00Z",
            "2026-02-29T00:00:00Z",
            "2026-04-31T00:00:00Z",
            "2026-11-11T24:00:00Z",
            "2026-11-11T23:60:00Z",
            "x026-11-11T16:56:59Z",
            "+026-11-11T16:56:59Z",
            "2026/11/11T16:56:59Z",
            "２026-11-11T16:56:59Z",
        ] {
            assert_eq!(parse_timestamp_ms(bad), None, "{bad}");
        }
    }
}
