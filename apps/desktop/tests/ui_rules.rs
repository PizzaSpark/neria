//! Guardrails for the Slint UI (`docs/DESIGN.md`): new screens and controls keep the rules
//! the UI foundation set up, without anyone having to remember them. Plain text checks over
//! `ui/**/*.slint`, so they run in a second with `cargo test -p neria --test ui_rules`.
//!
//! When a check fails, the message says what to do. The escape hatch for a real exception is
//! a comment on the line itself or just above it: `// ui-rules: <why>`.

use std::fs;
use std::path::{Path, PathBuf};

fn ui_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("ui")
}

/// Every .slint file under ui/, with its path relative to ui/ (forward slashes) and lines.
fn slint_files() -> Vec<(String, Vec<String>)> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for e in fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().is_some_and(|x| x == "slint") {
                out.push(p);
            }
        }
    }
    let mut paths = Vec::new();
    walk(&ui_dir(), &mut paths);
    paths.sort();
    paths
        .into_iter()
        .map(|p| {
            let rel = p.strip_prefix(ui_dir()).unwrap().to_string_lossy().replace('\\', "/");
            let text = fs::read_to_string(&p).unwrap();
            (rel, text.lines().map(str::to_string).collect())
        })
        .collect()
}

/// The line (or the 2 above it) says why it's an exception.
fn excused(lines: &[String], i: usize) -> bool {
    (i.saturating_sub(2)..=i).any(|k| lines[k].contains("ui-rules:"))
}

/// The lines of the `{ … }` block that opens on line `i`.
fn block(lines: &[String], i: usize) -> &[String] {
    let mut depth = 0i32;
    for (k, l) in lines.iter().enumerate().skip(i) {
        depth += l.matches('{').count() as i32 - l.matches('}').count() as i32;
        if depth <= 0 {
            return &lines[i..=k];
        }
    }
    &lines[i..]
}

fn fail_if_any(problems: Vec<String>, what: &str) {
    assert!(problems.is_empty(), "\n{what}\n\n{}\n", problems.join("\n"));
}

#[test]
fn colours_come_from_the_theme() {
    let mut problems = Vec::new();
    for (file, lines) in slint_files() {
        if file == "theme.slint" {
            continue;
        }
        for (i, l) in lines.iter().enumerate() {
            let code = l.split("//").next().unwrap_or("");
            if code.contains('#')
                && code.split('#').skip(1).any(|h| h.chars().take(6).all(|c| c.is_ascii_hexdigit()) && h.len() >= 6)
                && !excused(&lines, i)
            {
                problems.push(format!("  {file}:{}: {}", i + 1, l.trim()));
            }
        }
    }
    fail_if_any(
        problems,
        "Colours belong in ui/theme.slint (with a dark and a light value), so both themes and \
         the contrast check below cover them. Add a token there and use Theme.<name>:",
    );
}

#[test]
fn everything_clickable_works_from_the_keyboard() {
    // Inside widgets.slint the controls carry `KeyPress`; elsewhere a bare TouchArea with a
    // click needs KeyPress next to it, or a comment saying how the keyboard reaches the action.
    let mut problems = Vec::new();
    for (file, lines) in slint_files() {
        for (i, l) in lines.iter().enumerate() {
            if !l.contains("TouchArea {") || !block(&lines, i).iter().any(|b| b.contains("clicked")) {
                continue;
            }
            // KeyPress anywhere in the same component, or a comment just above.
            let start = (0..i)
                .rev()
                .find(|&k| lines[k].starts_with("export component") || lines[k].starts_with("component "))
                .unwrap_or(0);
            let near = (start..i).any(|k| lines[k].contains("KeyPress {"))
                || (i.saturating_sub(3)..i).any(|k| lines[k].contains("keyboard:"));
            if !near && !excused(&lines, i) {
                problems.push(format!("  {file}:{}: {}", i + 1, l.trim()));
            }
        }
    }
    fail_if_any(
        problems,
        "A TouchArea with `clicked` isn't reachable with Tab. Build the control from the shared \
         widgets (they have KeyPress), add `KeyPress { activated => … }` next to it, or, when a \
         button nearby already does the same thing, say so in a comment just above: \
         `// keyboard: the chevron button does this`.",
    );
}

#[test]
fn icon_only_buttons_have_a_spoken_label() {
    let mut problems = Vec::new();
    for (file, lines) in slint_files() {
        if file == "widgets.slint" {
            continue;
        }
        for (i, l) in lines.iter().enumerate() {
            let t = l.trim_start();
            let icon_button = t.starts_with("IconButton {") || t.contains(": IconButton {");
            let ghost = t.starts_with("GhostButton {") || t.contains(": GhostButton {");
            if !icon_button && !ghost {
                continue;
            }
            let b = block(&lines, i);
            // Only the button's own lines (not nested elements).
            let own: Vec<&String> = b.iter().skip(1).take_while(|x| !x.trim_end().ends_with('{')).collect();
            let has_label = own.iter().any(|x| x.trim_start().starts_with("label:"));
            let icon_only = icon_button
                || own.iter().any(|x| {
                    let x = x.trim_start();
                    x.starts_with("text:") && x.contains("\"\"")
                })
                || !own.iter().any(|x| x.trim_start().starts_with("text:"));
            if icon_only && !has_label && !excused(&lines, i) {
                problems.push(format!("  {file}:{}: {}", i + 1, l.trim()));
            }
        }
    }
    fail_if_any(
        problems,
        "A button that shows only an icon (on some widths or always) needs `label: \"…\"`, which \
         is what screen readers say:",
    );
}

#[test]
fn nothing_is_wider_than_the_smallest_window() {
    // The window can be 400px wide (compact layout, and Android later). A fixed width bigger
    // than 360px on a page, component or dialog will clip there. Dialog `card-width` is fine
    // (the card shrinks to the window).
    let mut problems = Vec::new();
    for (file, lines) in slint_files() {
        if file == "app.slint" || file == "theme.slint" {
            continue;
        }
        for (i, l) in lines.iter().enumerate() {
            let t = l.trim_start();
            if t.starts_with("card-width") || t.contains("min(") || t.contains("max(") {
                continue;
            }
            for key in ["width:", "min-width:", "preferred-width:"] {
                if let Some(rest) = t.strip_prefix(key) {
                    let n: String = rest.trim().chars().take_while(|c| c.is_ascii_digit()).collect();
                    if rest.trim().starts_with(&format!("{n}px"))
                        && n.parse::<u32>().is_ok_and(|n| n > 360)
                        && !excused(&lines, i)
                    {
                        problems.push(format!("  {file}:{}: {}", i + 1, l.trim()));
                    }
                }
            }
        }
    }
    fail_if_any(
        problems,
        "These fixed widths are wider than the 400px window allows. Use min(…, parent.width) or \
         let the layout size it, and check with NERIA_WINDOW=400x600:",
    );
}

// ---- Contrast of the theme tokens (WCAG 2.1) ----

fn hex(s: &str) -> Option<(f64, f64, f64)> {
    let h = s.trim().trim_start_matches('#');
    if h.len() < 6 {
        return None;
    }
    let c = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).ok().map(|v| v as f64 / 255.0);
    Some((c(0)?, c(2)?, c(4)?))
}

fn luminance((r, g, b): (f64, f64, f64)) -> f64 {
    let f = |c: f64| if c <= 0.03928 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) };
    0.2126 * f(r) + 0.7152 * f(g) + 0.0722 * f(b)
}

fn ratio(a: (f64, f64, f64), b: (f64, f64, f64)) -> f64 {
    let (x, y) = (luminance(a), luminance(b));
    (x.max(y) + 0.05) / (x.min(y) + 0.05)
}

/// (dark, light) value of each `out property <color> name: dark ? #… : #…;` (or a single value).
fn tokens() -> std::collections::HashMap<String, (String, String)> {
    let text = fs::read_to_string(ui_dir().join("theme.slint")).unwrap();
    let mut out = std::collections::HashMap::new();
    for l in text.lines() {
        let Some(rest) = l.trim().strip_prefix("out property <color> ") else { continue };
        let Some((name, value)) = rest.split_once(':') else { continue };
        let value = value.trim().trim_end_matches(';');
        let (d, li) = match value.strip_prefix("dark ?") {
            Some(v) => match v.split_once(':') {
                Some((d, l)) => (d.trim().to_string(), l.trim().to_string()),
                None => continue,
            },
            None => (value.to_string(), value.to_string()),
        };
        if d.starts_with('#') && li.starts_with('#') {
            out.insert(name.trim().to_string(), (d, li));
        }
    }
    out
}

#[test]
fn theme_colours_keep_their_contrast() {
    let t = tokens();
    let get = |name: &str, dark: bool| {
        let (d, l) = t.get(name).unwrap_or_else(|| panic!("theme token {name} not found"));
        hex(if dark { d } else { l }).unwrap()
    };
    // (text token, surfaces, minimum ratio, what for)
    let rules: &[(&str, &[&str], f64, &str)] = &[
        ("ink", &["bg", "panel", "raised"], 4.5, "body text"),
        ("muted", &["bg", "panel", "raised"], 4.5, "secondary text, hints, placeholders"),
        ("pop-text", &["bg", "panel"], 4.5, "accent text"),
        ("dim", &["bg", "panel", "raised"], 3.0, "icons and disabled labels (never body text)"),
        ("edge", &["bg", "panel", "raised"], 3.0, "outlines that alone show a control"),
        ("focus", &["bg", "panel", "raised"], 3.0, "the keyboard focus ring"),
        ("ok", &["ok-bg"], 4.5, "status tags"),
        ("warn", &["warn-bg"], 4.5, "status tags"),
        ("bad", &["bad-bg"], 4.5, "status tags"),
        ("on-pop", &["pop"], 4.5, "text on the yellow main button"),
    ];
    let mut problems = Vec::new();
    for dark in [true, false] {
        for (fg, bgs, min, what) in rules {
            for bg in *bgs {
                let r = ratio(get(fg, dark), get(bg, dark));
                if r < *min {
                    problems.push(format!(
                        "  {} theme: {fg} on {bg} is {r:.2}:1, needs {min}:1 ({what})",
                        if dark { "dark" } else { "light" }
                    ));
                }
            }
        }
    }
    fail_if_any(problems, "Theme colours lost contrast (ui/theme.slint):");
}
