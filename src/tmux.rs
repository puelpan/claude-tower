//! tmux: list panes, capture a pane, jump to a pane (§3.6, §7).
//!
//! tmux is always run with an argument vector, never through a shell, and
//! with a timeout. It talks to the server named by `$TMUX` (or the default
//! server when unset).

use std::io::{ErrorKind, Read};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use crate::model::PaneInfo;

/// One line of `tmux list-panes -a`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Pane {
    pub pane_id: String,
    pub pane_pid: u32,
    pub session_id: String,
    pub session_name: String,
    pub window_id: String,
    pub window_index: u32,
    pub window_name: String,
    pub pane_index: u32,
    pub pane_active: bool,
    pub window_active: bool,
    /// Number of clients attached to the session.
    pub session_attached: u32,
    /// Last field, so a tab inside a title can't shift the others.
    pub title: String,
}

impl Pane {
    /// Someone is looking at it: active pane of the active window of an
    /// attached session (§4 "new").
    pub fn is_seen(&self) -> bool {
        self.pane_active && self.window_active && self.session_attached > 0
    }

    pub fn info(&self) -> PaneInfo {
        PaneInfo {
            pane_id: self.pane_id.clone(),
            session_id: self.session_id.clone(),
            session_name: self.session_name.clone(),
            window_id: self.window_id.clone(),
            window_index: self.window_index,
            window_name: self.window_name.clone(),
            pane_index: self.pane_index,
            title: self.title.clone(),
        }
    }
}

/// The `-F` format for `list-panes`, tab-separated, `pane_title` last.
pub const LIST_PANES_FORMAT: &str = "#{pane_id}\t#{pane_pid}\t#{session_id}\t#{session_name}\t#{window_id}\t#{window_index}\t#{window_name}\t#{pane_index}\t#{pane_active}\t#{window_active}\t#{session_attached}\t#{pane_title}";

/// Parses `list-panes` output; lines that don't parse are skipped. The
/// title is everything after the 11th tab.
pub fn parse_panes(out: &str) -> Vec<Pane> {
    out.lines().filter_map(parse_pane).collect()
}

fn parse_pane(line: &str) -> Option<Pane> {
    let f: Vec<&str> = line.splitn(12, '\t').collect();
    if f.len() != 12 {
        return None;
    }
    let pane = Pane {
        pane_id: f[0].to_string(),
        pane_pid: f[1].parse().ok()?,
        session_id: f[2].to_string(),
        session_name: f[3].to_string(),
        window_id: f[4].to_string(),
        window_index: f[5].parse().ok()?,
        window_name: f[6].to_string(),
        pane_index: f[7].parse().ok()?,
        pane_active: f[8] == "1",
        window_active: f[9] == "1",
        session_attached: f[10].parse().ok()?,
        title: f[11].to_string(),
    };
    let ok = pane.pane_id.starts_with('%')
        && pane.session_id.starts_with('$')
        && pane.window_id.starts_with('@');
    ok.then_some(pane)
}

/// One attached client.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Client {
    /// e.g. `/dev/pts/7`.
    pub name: String,
    pub session_id: String,
    pub pid: u32,
    /// `client_activity`, seconds since epoch.
    pub activity: i64,
}

pub const LIST_CLIENTS_FORMAT: &str =
    "#{client_name}\t#{session_id}\t#{client_pid}\t#{client_activity}";

/// Parses `list-clients` output; lines that don't parse are skipped.
pub fn parse_clients(out: &str) -> Vec<Client> {
    out.lines()
        .filter_map(|line| {
            let mut parts = line.split('\t');
            let client = Client {
                name: parts.next()?.to_string(),
                session_id: parts.next()?.to_string(),
                pid: parts.next()?.parse().ok()?,
                activity: parts.next()?.parse().ok()?,
            };
            (!client.name.is_empty() && parts.next().is_none()).then_some(client)
        })
        .collect()
}

/// The most recently active client; the first one listed on a tie.
pub fn pick_client(clients: &[Client]) -> Option<&Client> {
    clients
        .iter()
        .reduce(|best, c| if c.activity > best.activity { c } else { best })
}

/// One tmux invocation that switches `client` to the pane: `switch-client
/// -c <client> -t <session_id> ; select-window -t <window_id> ; select-pane
/// -t <pane_id>` (§7).
pub fn jump_args(client: &str, pane: &PaneInfo) -> Vec<String> {
    [
        "switch-client",
        "-c",
        client,
        "-t",
        &pane.session_id,
        ";",
        "select-window",
        "-t",
        &pane.window_id,
        ";",
        "select-pane",
        "-t",
        &pane.pane_id,
    ]
    .map(str::to_string)
    .to_vec()
}

/// True when running inside tmux (`$TMUX` set and non-empty).
pub fn in_tmux() -> bool {
    std::env::var_os("TMUX").is_some_and(|v| !v.is_empty())
}

/// Drops trailing lines that are empty or only whitespace.
fn trim_trailing_blank_lines(text: &str) -> &str {
    let mut end = 0;
    let mut pos = 0;
    for line in text.split_inclusive('\n') {
        pos += line.len();
        if !line.trim().is_empty() {
            end = pos;
        }
    }
    text[..end].trim_end_matches(['\n', '\r'])
}

/// Runs the `tmux` binary.
#[derive(Debug, Clone)]
pub struct Tmux {
    timeout: Duration,
    /// `-L <name>`: a private server instead of the one in `$TMUX`.
    socket: Option<String>,
}

impl Tmux {
    pub fn new(timeout: Duration) -> Tmux {
        Tmux {
            timeout,
            socket: None,
        }
    }

    /// Talks to the server on socket `name` (`tmux -L <name>`), e.g. a
    /// private one for tests.
    pub fn with_socket(timeout: Duration, socket_name: &str) -> Tmux {
        Tmux {
            timeout,
            socket: Some(socket_name.to_string()),
        }
    }

    /// Runs tmux with `args`; stdout on success, a one-line error otherwise
    /// (including "tmux timed out" and "tmux not found").
    pub fn run<S: AsRef<str>>(&self, args: &[S]) -> Result<String, String> {
        let mut cmd = Command::new("tmux");
        if let Some(socket) = &self.socket {
            cmd.args(["-L", socket]);
        }
        let mut child = cmd
            .args(args.iter().map(AsRef::as_ref))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| match e.kind() {
                ErrorKind::NotFound => "tmux not found".to_string(),
                _ => format!("running tmux: {e}"),
            })?;
        let deadline = Instant::now() + self.timeout;
        // Read on threads so a full pipe can't block tmux.
        let out = read_pipe(child.stdout.take());
        let err = read_pipe(child.stderr.take());
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(2))
                }
                result => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(match result {
                        Err(e) => format!("waiting for tmux: {e}"),
                        _ => "tmux timed out".to_string(),
                    });
                }
            }
        };
        // The pipes close when tmux exits; a little grace for the readers.
        let wait = deadline.saturating_duration_since(Instant::now()) + Duration::from_millis(100);
        let out = out.recv_timeout(wait).map_err(|_| "tmux timed out")?;
        let err = err.recv_timeout(wait).map_err(|_| "tmux timed out")?;
        if status.success() {
            return Ok(String::from_utf8_lossy(&out).into_owned());
        }
        let err = String::from_utf8_lossy(&err);
        let err = err
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect::<Vec<_>>()
            .join("; ");
        let sub = args.first().map_or("", AsRef::as_ref);
        if err.is_empty() {
            Err(format!("tmux {sub}: {status}"))
        } else {
            Err(format!("tmux {sub}: {err}"))
        }
    }

    /// Every pane of every session.
    pub fn list_panes(&self) -> Result<Vec<Pane>, String> {
        let out = self.run(&["list-panes", "-a", "-F", LIST_PANES_FORMAT])?;
        Ok(parse_panes(&out))
    }

    /// `capture-pane -p -e -J -t <pane> -S -<lines>`.
    pub fn capture(&self, pane_id: &str, lines: u32) -> Result<String, String> {
        let start = format!("-{lines}");
        let out = self.run(&[
            "capture-pane",
            "-p",
            "-e",
            "-J",
            "-t",
            pane_id,
            "-S",
            &start,
        ])?;
        Ok(trim_trailing_blank_lines(&out).to_string())
    }

    /// Clients, optionally only those attached to `session` (`-t`).
    pub fn list_clients(&self, session: Option<&str>) -> Result<Vec<Client>, String> {
        let mut args = vec!["list-clients", "-F", LIST_CLIENTS_FORMAT];
        if let Some(session) = session {
            args.extend(["-t", session]);
        }
        Ok(parse_clients(&self.run(&args)?))
    }

    /// The client to switch on Enter (§7). In popup mode,
    /// `display-message -p '#{client_name}'`. Otherwise the clients of the
    /// session holding this process's pane (`$TMUX_PANE`), most recent
    /// `client_activity` first.
    pub fn dashboard_client(&self, popup: bool) -> Result<String, String> {
        if popup {
            let out = self.run(&["display-message", "-p", "#{client_name}"])?;
            let name = out.trim();
            if name.is_empty() {
                return Err("tmux gave no client for the popup".into());
            }
            return Ok(name.to_string());
        }
        let pane = std::env::var("TMUX_PANE").unwrap_or_default();
        if pane.is_empty() {
            return Err("not inside tmux".into());
        }
        let out = self.run(&["display-message", "-p", "-t", &pane, "#{session_id}"])?;
        let session = out.trim();
        if session.is_empty() {
            return Err(format!("no tmux session for pane {pane}"));
        }
        let clients = self.list_clients(Some(session))?;
        pick_client(&clients)
            .map(|c| c.name.clone())
            .ok_or_else(|| "no tmux client is showing the dashboard".to_string())
    }

    /// Switches `client` to `pane` in one tmux call.
    pub fn jump(&self, client: &str, pane: &PaneInfo) -> Result<(), String> {
        self.run(&jump_args(client, pane)).map(drop)
    }
}

/// Reads all of `pipe` on its own thread; the bytes arrive on the channel.
fn read_pipe<R: Read + Send + 'static>(pipe: Option<R>) -> Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    match pipe {
        Some(mut pipe) => {
            std::thread::spawn(move || {
                let mut buf = Vec::new();
                let _ = pipe.read_to_end(&mut buf);
                let _ = tx.send(buf);
            });
        }
        None => {
            let _ = tx.send(Vec::new());
        }
    }
    rx
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(fields: &[&str]) -> String {
        fields.join("\t")
    }

    #[test]
    fn panes() {
        let out = [
            line(&[
                "%3",
                "1234",
                "$1",
                "shop-api",
                "@3",
                "2",
                "claude",
                "0",
                "1",
                "1",
                "2",
                "✳ Issue\t882\tdone",
            ]),
            // Bad number.
            line(&[
                "%4", "x", "$1", "s", "@3", "2", "w", "1", "0", "1", "1", "t",
            ]),
            // Ids without their prefix.
            line(&["5", "1", "$1", "s", "@3", "2", "w", "1", "0", "1", "1", "t"]),
            line(&["%5", "1", "1", "s", "@3", "2", "w", "1", "0", "1", "1", "t"]),
            line(&["%5", "1", "$1", "s", "3", "2", "w", "1", "0", "1", "1", "t"]),
            // Too few fields.
            "%6\t1\t$1".to_string(),
            "bad line".to_string(),
            line(&[
                "%7", "99", "$2", "etl jobs", "@9", "0", "w", "3", "0", "1", "0", "",
            ]),
        ]
        .join("\n");
        let panes = parse_panes(&out);
        assert_eq!(panes.len(), 2);
        assert_eq!(
            panes[0],
            Pane {
                pane_id: "%3".into(),
                pane_pid: 1234,
                session_id: "$1".into(),
                session_name: "shop-api".into(),
                window_id: "@3".into(),
                window_index: 2,
                window_name: "claude".into(),
                pane_index: 0,
                pane_active: true,
                window_active: true,
                session_attached: 2,
                title: "✳ Issue\t882\tdone".into(),
            }
        );
        assert!(panes[0].is_seen());
        let p = &panes[1];
        assert_eq!(p.pane_id, "%7");
        assert_eq!(p.session_name, "etl jobs");
        assert_eq!(p.pane_index, 3);
        assert!(!p.pane_active);
        assert!(p.window_active);
        assert_eq!(p.session_attached, 0);
        assert_eq!(p.title, "");
        assert!(!p.is_seen());
        assert_eq!(p.info().window_id, "@9");
    }

    #[test]
    fn clients() {
        let out = "/dev/pts/36\t$3\t762724\t1794335934\nbad line\n/dev/pts/7\t$1\tx\t1\n\
                   /dev/pts/8\t$1\t42\t5\n\t$1\t1\t1\n/dev/pts/9\t$1\t1\t1\textra\n";
        let clients = parse_clients(out);
        assert_eq!(
            clients,
            vec![
                Client {
                    name: "/dev/pts/36".into(),
                    session_id: "$3".into(),
                    pid: 762724,
                    activity: 1794335934,
                },
                Client {
                    name: "/dev/pts/8".into(),
                    session_id: "$1".into(),
                    pid: 42,
                    activity: 5,
                },
            ]
        );
    }

    #[test]
    fn pick_most_recent_client() {
        assert_eq!(pick_client(&[]), None);
        let client = |name: &str, activity| Client {
            name: name.into(),
            activity,
            ..Client::default()
        };
        let clients = [
            client("a", 5),
            client("b", 9),
            client("c", 9),
            client("d", 1),
        ];
        assert_eq!(pick_client(&clients).unwrap().name, "b");
    }

    #[test]
    fn jump_is_one_invocation() {
        let pane = PaneInfo {
            pane_id: "%12".into(),
            session_id: "$3".into(),
            window_id: "@5".into(),
            ..PaneInfo::default()
        };
        assert_eq!(
            jump_args("/dev/pts/36", &pane),
            [
                "switch-client",
                "-c",
                "/dev/pts/36",
                "-t",
                "$3",
                ";",
                "select-window",
                "-t",
                "@5",
                ";",
                "select-pane",
                "-t",
                "%12",
            ]
        );
    }

    #[test]
    fn trims_trailing_blank_lines() {
        assert_eq!(trim_trailing_blank_lines("a\n\nb\n  \n\n"), "a\n\nb");
        assert_eq!(trim_trailing_blank_lines("a\r\n \r\n"), "a");
        assert_eq!(trim_trailing_blank_lines("\n\n"), "");
        assert_eq!(trim_trailing_blank_lines(""), "");
        assert_eq!(trim_trailing_blank_lines("  x"), "  x");
    }

    /// Kills the private test server and removes its socket, even when the
    /// test fails.
    struct Server(Tmux);

    impl Drop for Server {
        fn drop(&mut self) {
            let socket = self.0.run(&["display-message", "-p", "#{socket_path}"]);
            let _ = self.0.run(&["kill-server"]);
            if let Ok(path) = socket {
                let _ = std::fs::remove_file(path.trim());
            }
        }
    }

    fn tmux_installed() -> bool {
        Command::new("tmux")
            .arg("-V")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok()
    }

    /// Polls `capture` until `want` shows up or a few seconds pass.
    fn capture_until(tmux: &Tmux, pane: &str, lines: u32, want: &str) -> String {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let text = tmux.capture(pane, lines).unwrap();
            if text.contains(want) || Instant::now() > deadline {
                return text;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn live_private_server() {
        if !tmux_installed() {
            eprintln!("tmux not installed; skipping");
            return;
        }
        let socket = format!("claude-tower-test-{}", std::process::id());
        let tmux = Tmux::with_socket(Duration::from_secs(5), &socket);
        let _server = Server(tmux.clone());
        // `-f /dev/null`: don't load the user's config into the test server.
        let started = Command::new("tmux")
            .args(["-L", &socket, "-f", "/dev/null"])
            .args(["new-session", "-d", "-s", "t1", "-n", "w1"])
            .args(["sh", "-c", "printf 'hello\\n'; sleep 30"])
            .stdin(Stdio::null())
            .status()
            .unwrap();
        assert!(started.success());

        let panes = tmux.list_panes().unwrap();
        assert_eq!(panes.len(), 1);
        let pane = &panes[0];
        assert!(pane.pane_id.starts_with('%'));
        assert!(pane.session_id.starts_with('$'));
        assert!(pane.window_id.starts_with('@'));
        assert_eq!(pane.session_name, "t1");
        assert_eq!(pane.window_name, "w1");
        assert!(pane.pane_pid > 0);
        assert!(pane.pane_active && pane.window_active);
        assert_eq!(pane.session_attached, 0);

        let text = capture_until(&tmux, &pane.pane_id, 10, "hello");
        assert_eq!(text, "hello");

        // Output bigger than a pipe buffer (64 KiB) must not block the reader.
        let big = "x".repeat(100);
        let script = format!(
            "i=0; while [ $i -lt 2000 ]; do echo \"$i {big}\"; i=$((i+1)); done; \
             echo done; sleep 30"
        );
        tmux.run(&[
            "new-window",
            "-d",
            "-t",
            "t1",
            "-n",
            "big",
            "sh",
            "-c",
            &script,
        ])
        .unwrap();
        let big_pane = tmux
            .list_panes()
            .unwrap()
            .into_iter()
            .find(|p| p.window_name == "big")
            .unwrap();
        let text = capture_until(&tmux, &big_pane.pane_id, 3000, "\ndone");
        assert!(text.len() > 64 * 1024, "captured {} bytes", text.len());
        assert!(text.ends_with("done"));

        let err = tmux.run(&["no-such-command"]).unwrap_err();
        assert!(err.starts_with("tmux no-such-command: "), "{err}");
        assert!(!err.contains('\n'));

        // `wait-for` blocks until signalled, so it can only time out.
        let short = Tmux::with_socket(Duration::from_millis(200), &socket);
        assert_eq!(
            short.run(&["wait-for", "never"]),
            Err("tmux timed out".into())
        );
    }
}
