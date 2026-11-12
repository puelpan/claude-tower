//! `/proc`: liveness and parent processes (§3.6, §3.7).

/// The fields of `/proc/<pid>/stat` that claude-tower uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stat {
    pub pid: u32,
    /// Field 4.
    pub ppid: u32,
    /// Field 22: start time in clock ticks since boot.
    pub start_ticks: u64,
}

/// Parses a `/proc/<pid>/stat` line. Field 2 (the process name, in
/// parentheses) can contain spaces and `)`, so fields are counted after the
/// last `)`.
pub fn parse_stat(line: &str) -> Option<Stat> {
    parse_with_state(line).map(|(stat, _)| stat)
}

/// `parse_stat` plus field 3, the state letter.
fn parse_with_state(line: &str) -> Option<(Stat, char)> {
    let open = line.find('(')?;
    let close = line.rfind(')')?;
    if close < open {
        return None;
    }
    let pid = line[..open].trim().parse().ok()?;
    // After the name: field 3 is index 0, so field n is index n - 3.
    let rest: Vec<&str> = line[close + 1..].split_ascii_whitespace().collect();
    let state = rest.first()?.chars().next()?;
    let ppid = rest.get(1)?.parse().ok()?;
    let start_ticks = rest.get(19)?.parse().ok()?;
    Some((
        Stat {
            pid,
            ppid,
            start_ticks,
        },
        state,
    ))
}

fn read_with_state(pid: u32) -> Option<(Stat, char)> {
    let bytes = std::fs::read(format!("/proc/{pid}/stat")).ok()?;
    // The name may not be UTF-8; the fields that matter are ASCII.
    parse_with_state(&String::from_utf8_lossy(&bytes))
}

/// Reads and parses `/proc/<pid>/stat`.
pub fn read_stat(pid: u32) -> Option<Stat> {
    read_with_state(pid).map(|(stat, _)| stat)
}

/// True if `pid` is running and, when `proc_start` is given, its start time
/// equals it (a reused pid is not alive). `proc_start` that isn't a number
/// never matches. A zombie is not running.
pub fn is_alive(pid: u32, proc_start: Option<&str>) -> bool {
    let want = match proc_start {
        Some(s) => match s.trim().parse::<u64>() {
            Ok(n) => Some(n),
            Err(_) => return false,
        },
        None => None,
    };
    let Some((stat, state)) = read_with_state(pid) else {
        return false;
    };
    if matches!(state, 'Z' | 'X' | 'x') {
        return false;
    }
    want.is_none_or(|n| n == stat.start_ticks)
}

/// Parent, grandparent, … of `pid` (not `pid` itself), stopping before pid
/// 0 or 1, at a missing process, at a cycle, or after `max` steps.
pub fn ancestors(pid: u32, max: usize) -> Vec<u32> {
    walk_up(pid, max, read_stat)
}

fn walk_up(pid: u32, max: usize, stat_of: impl Fn(u32) -> Option<Stat>) -> Vec<u32> {
    let mut out = Vec::new();
    let mut cur = pid;
    while out.len() < max {
        let Some(stat) = stat_of(cur) else { break };
        let ppid = stat.ppid;
        if ppid <= 1 || ppid == pid || out.contains(&ppid) {
            break;
        }
        out.push(ppid);
        cur = ppid;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stat line with `name` in field 2, ppid 77 and starttime 123456.
    fn line(name: &str) -> String {
        format!(
            "4242 ({name}) S 77 4242 4242 0 -1 4194560 1234 0 0 0 10 5 0 0 20 0 1 0 \
             123456 12345678 900 18446744073709551615 0 0 0 0 0 0 0 0 0 0 0 0 17 3 0 0\n"
        )
    }

    #[test]
    fn plain_name() {
        let want = Stat {
            pid: 4242,
            ppid: 77,
            start_ticks: 123456,
        };
        assert_eq!(parse_stat(&line("claude")), Some(want));
        assert_eq!(parse_with_state(&line("claude")).unwrap().1, 'S');
    }

    #[test]
    fn tricky_names() {
        for name in [
            "(a) b)",
            "(x (y))",
            "a b c",
            ") S 1 2 3",
            "1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20",
            "",
            "((",
            "))",
        ] {
            let s = parse_stat(&line(name)).unwrap_or_else(|| panic!("{name:?}"));
            assert_eq!(
                (s.pid, s.ppid, s.start_ticks),
                (4242, 77, 123456),
                "{name:?}"
            );
        }
    }

    #[test]
    fn real_line_without_newline() {
        let s = "1228510 (claude) S 1228400 1228510 1228400 34827 1228510 4194304 \
                 82312 0 0 0 5810 1203 0 0 20 0 18 0 13982212 75023360000 89000";
        let st = parse_stat(s).unwrap();
        assert_eq!(
            (st.pid, st.ppid, st.start_ticks),
            (1228510, 1228400, 13982212)
        );
    }

    #[test]
    fn malformed() {
        assert_eq!(parse_stat(""), None);
        assert_eq!(parse_stat("4242"), None);
        assert_eq!(parse_stat("4242 (claude"), None);
        assert_eq!(parse_stat("4242 claude) S 1"), None);
        assert_eq!(parse_stat(&line("c").replacen("4242", "x", 1)), None);
        // Too few fields for field 22.
        assert_eq!(parse_stat("4242 (claude) S 77 1 2 3"), None);
        // Non-numeric ppid.
        assert_eq!(parse_stat(&line("c").replacen(" 77 ", " p ", 1)), None);
    }

    #[test]
    fn current_process() {
        let me = std::process::id();
        let st = read_stat(me).unwrap();
        assert_eq!(st.pid, me);
        assert_eq!(st.ppid, std::os::unix::process::parent_id());

        let start = st.start_ticks.to_string();
        assert!(is_alive(me, None));
        assert!(is_alive(me, Some(&start)));
        assert!(!is_alive(me, Some(&(st.start_ticks + 1).to_string())));
        assert!(!is_alive(me, Some("abc")));
        assert!(!is_alive(me, Some("")));
    }

    #[test]
    fn missing_process() {
        // Above the kernel's pid_max (4194304 at most).
        assert!(read_stat(u32::MAX).is_none());
        assert!(!is_alive(u32::MAX, None));
        assert!(!is_alive(u32::MAX, Some("1")));
        assert!(ancestors(u32::MAX, 10).is_empty());
    }

    #[test]
    fn ancestors_of_current_process() {
        let me = std::process::id();
        let parent = std::os::unix::process::parent_id();
        let up = ancestors(me, 64);
        assert!(!up.contains(&me));
        assert!(!up.contains(&1));
        if parent > 1 {
            assert_eq!(up.first(), Some(&parent));
        }
        assert!(ancestors(me, 0).is_empty());
        assert!(ancestors(me, 1).len() <= 1);
    }

    fn table(pairs: &[(u32, u32)]) -> impl Fn(u32) -> Option<Stat> + '_ {
        move |pid| {
            pairs
                .iter()
                .find(|(p, _)| *p == pid)
                .map(|&(pid, ppid)| Stat {
                    pid,
                    ppid,
                    start_ticks: 0,
                })
        }
    }

    #[test]
    fn walk_stops() {
        // Chain to init.
        let t = [(10, 9), (9, 8), (8, 1)];
        assert_eq!(walk_up(10, 10, table(&t)), [9, 8]);
        // Max steps.
        assert_eq!(walk_up(10, 1, table(&t)), [9]);
        // Missing parent.
        let t = [(10, 9), (9, 8)];
        assert_eq!(walk_up(10, 10, table(&t)), [9, 8]);
        // Parent 0 (outside the pid namespace).
        let t = [(10, 9), (9, 0)];
        assert_eq!(walk_up(10, 10, table(&t)), [9]);
        // Cycles.
        let t = [(10, 9), (9, 8), (8, 9)];
        assert_eq!(walk_up(10, 10, table(&t)), [9, 8]);
        let t = [(10, 9), (9, 10)];
        assert_eq!(walk_up(10, 10, table(&t)), [9]);
        let t = [(10, 10)];
        assert!(walk_up(10, 10, table(&t)).is_empty());
    }
}
