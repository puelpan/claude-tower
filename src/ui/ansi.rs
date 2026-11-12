//! Turns `capture-pane -e` output (text with SGR escapes) into styled lines.
//! SGR attributes and colors are kept; every other escape sequence and
//! control character is dropped.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

const ESC: char = '\x1b';

/// Parses `text` into lines. The style carries over from one line to the
/// next, as in the terminal. A trailing newline doesn't add an empty line.
pub fn to_lines(text: &str) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut buf = String::new();
    let mut style = Style::default();
    let mut chars = text.chars().peekable();

    while let Some(c) = chars.next() {
        match c {
            '\n' => {
                flush(&mut spans, &mut buf, style);
                lines.push(Line::from(std::mem::take(&mut spans)));
            }
            ESC => match chars.next() {
                // CSI: parameters, intermediates, one final byte.
                Some('[') => {
                    let mut params = String::new();
                    let mut final_byte = None;
                    for c in chars.by_ref() {
                        if ('\x40'..='\x7e').contains(&c) {
                            final_byte = Some(c);
                            break;
                        }
                        params.push(c);
                    }
                    if final_byte == Some('m') {
                        let next = apply_sgr(style, &params);
                        if next != style {
                            flush(&mut spans, &mut buf, style);
                            style = next;
                        }
                    }
                }
                // OSC, DCS, SOS, PM, APC: up to BEL or ST (ESC \).
                Some(']' | 'P' | 'X' | '^' | '_') => {
                    while let Some(c) = chars.next() {
                        if c == '\x07' {
                            break;
                        }
                        if c == ESC {
                            if chars.peek() == Some(&'\\') {
                                chars.next();
                            }
                            break;
                        }
                    }
                }
                // Other escapes: intermediates, then one final byte.
                Some(c) if ('\x20'..='\x2f').contains(&c) => {
                    for c in chars.by_ref() {
                        if !('\x20'..='\x2f').contains(&c) {
                            break;
                        }
                    }
                }
                _ => {}
            },
            '\t' => buf.push(' '),
            c if c.is_control() => {}
            c => buf.push(c),
        }
    }
    flush(&mut spans, &mut buf, style);
    if !spans.is_empty() {
        lines.push(Line::from(spans));
    }
    lines
}

fn flush(spans: &mut Vec<Span<'static>>, buf: &mut String, style: Style) {
    if !buf.is_empty() {
        spans.push(Span::styled(std::mem::take(buf), style));
    }
}

/// Applies one SGR sequence's parameters (`1;31`, `38;5;208`, `38:2::1:2:3`).
fn apply_sgr(mut style: Style, params: &str) -> Style {
    // Sub-parameters (`:`) belong to the code before them.
    let groups: Vec<Vec<Option<u32>>> = params
        .split(';')
        .map(|g| g.split(':').map(|n| n.parse().ok()).collect())
        .collect();
    let mut i = 0;
    while i < groups.len() {
        let group = &groups[i];
        let code = group[0].unwrap_or(0);
        i += 1;
        match code {
            0 => style = Style::default(),
            1 => style = add(style, Modifier::BOLD),
            2 => style = add(style, Modifier::DIM),
            3 => style = add(style, Modifier::ITALIC),
            4 => {
                // `4:0` turns underline off.
                if group.get(1).copied().flatten() == Some(0) {
                    style = remove(style, Modifier::UNDERLINED);
                } else {
                    style = add(style, Modifier::UNDERLINED);
                }
            }
            7 => style = add(style, Modifier::REVERSED),
            9 => style = add(style, Modifier::CROSSED_OUT),
            21 => style = add(style, Modifier::UNDERLINED),
            22 => style = remove(style, Modifier::BOLD | Modifier::DIM),
            23 => style = remove(style, Modifier::ITALIC),
            24 => style = remove(style, Modifier::UNDERLINED),
            27 => style = remove(style, Modifier::REVERSED),
            29 => style = remove(style, Modifier::CROSSED_OUT),
            30..=37 => style.fg = Some(named(code - 30)),
            39 => style.fg = None,
            40..=47 => style.bg = Some(named(code - 40)),
            49 => style.bg = None,
            90..=97 => style.fg = Some(named(code - 90 + 8)),
            100..=107 => style.bg = Some(named(code - 100 + 8)),
            38 | 48 | 58 => {
                let color = if group.len() > 1 {
                    extended(&group[1..])
                } else {
                    let rest: Vec<Option<u32>> = groups[i..].iter().map(|g| g[0]).collect();
                    let (color, used) = extended_semicolon(&rest);
                    i += used;
                    color
                };
                match (code, color) {
                    (38, Some(c)) => style.fg = Some(c),
                    (48, Some(c)) => style.bg = Some(c),
                    _ => {}
                }
            }
            _ => {}
        }
    }
    style
}

fn add(mut style: Style, m: Modifier) -> Style {
    style.add_modifier.insert(m);
    style.sub_modifier.remove(m);
    style
}

fn remove(mut style: Style, m: Modifier) -> Style {
    style.add_modifier.remove(m);
    style
}

/// `5;n` or `2;r;g;b` after a `38`/`48` written with semicolons; returns
/// the color and how many parameters it used.
fn extended_semicolon(rest: &[Option<u32>]) -> (Option<Color>, usize) {
    match rest.first().copied().flatten() {
        Some(5) => (rest.get(1).copied().flatten().map(indexed), 2),
        Some(2) => {
            let c = |i: usize| rest.get(i).copied().flatten().map(byte);
            let color = match (c(1), c(2), c(3)) {
                (Some(r), Some(g), Some(b)) => Some(Color::Rgb(r, g, b)),
                _ => None,
            };
            (color, 4)
        }
        _ => (None, 1),
    }
}

/// `5:n`, `2:r:g:b` or `2::r:g:b` (with an empty color space id).
fn extended(sub: &[Option<u32>]) -> Option<Color> {
    match sub.first().copied().flatten()? {
        5 => sub.get(1).copied().flatten().map(indexed),
        2 => {
            let rgb: Vec<u8> = if sub.len() >= 5 {
                sub[2..5].iter().map(|v| byte(v.unwrap_or(0))).collect()
            } else {
                sub[1..].iter().map(|v| byte(v.unwrap_or(0))).collect()
            };
            match rgb[..] {
                [r, g, b, ..] => Some(Color::Rgb(r, g, b)),
                _ => None,
            }
        }
        _ => None,
    }
}

fn byte(n: u32) -> u8 {
    n.min(255) as u8
}

/// 0–15 as the named colors, the rest as indexed.
fn indexed(n: u32) -> Color {
    let n = byte(n);
    if n < 16 {
        named(u32::from(n))
    } else {
        Color::Indexed(n)
    }
}

fn named(n: u32) -> Color {
    match n {
        0 => Color::Black,
        1 => Color::Red,
        2 => Color::Green,
        3 => Color::Yellow,
        4 => Color::Blue,
        5 => Color::Magenta,
        6 => Color::Cyan,
        7 => Color::Gray,
        8 => Color::DarkGray,
        9 => Color::LightRed,
        10 => Color::LightGreen,
        11 => Color::LightYellow,
        12 => Color::LightBlue,
        13 => Color::LightMagenta,
        14 => Color::LightCyan,
        _ => Color::White,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(lines: &[Line]) -> Vec<String> {
        lines.iter().map(|l| l.to_string()).collect()
    }

    #[test]
    fn plain_text_and_trailing_newline() {
        let lines = to_lines("one\ntwo\n");
        assert_eq!(plain(&lines), ["one", "two"]);
        assert_eq!(lines[0].spans[0].style, Style::default());
        assert_eq!(plain(&to_lines("a\n\nb")), ["a", "", "b"]);
        assert!(to_lines("").is_empty());
    }

    #[test]
    fn attributes_and_reset() {
        let lines = to_lines("\x1b[1mbold\x1b[22m \x1b[2;3;4;7mmix\x1b[0mplain\x1b[mx");
        let spans = &lines[0].spans;
        assert_eq!(spans[0].content, "bold");
        assert_eq!(spans[0].style, Style::new().add_modifier(Modifier::BOLD));
        assert_eq!(spans[1].content, " ");
        assert_eq!(spans[1].style, Style::default());
        assert_eq!(spans[2].content, "mix");
        assert_eq!(
            spans[2].style,
            Style::new().add_modifier(
                Modifier::DIM | Modifier::ITALIC | Modifier::UNDERLINED | Modifier::REVERSED
            )
        );
        assert_eq!(spans[3].content, "plainx");
        assert_eq!(spans[3].style, Style::default());
    }

    #[test]
    fn named_colors() {
        let lines = to_lines("\x1b[31mr\x1b[42mg\x1b[39mx\x1b[49m\x1b[94mb\x1b[103my");
        let s = &lines[0].spans;
        assert_eq!(s[0].style, Style::new().fg(Color::Red));
        assert_eq!(s[1].style, Style::new().fg(Color::Red).bg(Color::Green));
        assert_eq!(s[2].style, Style::new().bg(Color::Green));
        assert_eq!(s[3].style, Style::new().fg(Color::LightBlue));
        assert_eq!(
            s[4].style,
            Style::new().fg(Color::LightBlue).bg(Color::LightYellow)
        );
    }

    #[test]
    fn indexed_and_rgb_colors() {
        let lines = to_lines(
            "\x1b[38;5;208ma\x1b[48;5;1mb\x1b[38;2;10;20;30mc\x1b[48:2::1:2:3md\x1b[38:5:99me",
        );
        let s = &lines[0].spans;
        assert_eq!(s[0].style.fg, Some(Color::Indexed(208)));
        assert_eq!(s[1].style.bg, Some(Color::Red));
        assert_eq!(s[2].style.fg, Some(Color::Rgb(10, 20, 30)));
        assert_eq!(s[3].style.bg, Some(Color::Rgb(1, 2, 3)));
        assert_eq!(s[4].style.fg, Some(Color::Indexed(99)));
        assert_eq!(s[4].style.bg, Some(Color::Rgb(1, 2, 3)));
    }

    #[test]
    fn extended_color_followed_by_more_codes() {
        let lines = to_lines("\x1b[38;5;12;1mx");
        let s = &lines[0].spans[0].style;
        assert_eq!(s.fg, Some(Color::LightBlue));
        assert!(s.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn style_carries_across_lines() {
        let lines = to_lines("\x1b[36mone\ntwo\x1b[0m");
        assert_eq!(lines[1].spans[0].style.fg, Some(Color::Cyan));
    }

    #[test]
    fn drops_other_escapes_and_controls() {
        let text = "a\x1b[2Kb\x1b]8;;http://x\x07c\x1b]0;title\x1b\\d\x1b(Be\x1b=f\x07g\rh\x1bPdcs\x1b\\i\x7fj\tk";
        assert_eq!(plain(&to_lines(text)), ["abcdefghij k"]);
    }

    #[test]
    fn unterminated_sequences_dont_panic() {
        assert_eq!(plain(&to_lines("x\x1b[")), ["x"]);
        assert_eq!(plain(&to_lines("x\x1b]0;t")), ["x"]);
        assert_eq!(plain(&to_lines("x\x1b")), ["x"]);
        assert_eq!(plain(&to_lines("\x1b[38;5mx\x1b[38;2;1mY")), ["xY"]);
    }

    #[test]
    fn wide_and_emoji_text_kept() {
        assert_eq!(plain(&to_lines("\x1b[1m日本 🚀\x1b[0m")), ["日本 🚀"]);
    }
}
