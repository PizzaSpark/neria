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
        ("live", &["live-bg", "bg", "panel"], 4.5, "the REC tag and recording rows"),
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

// ---- Behaviour the shared controls promise (shadcn / Radix-style) ----

/// The direct children of the `{ … }` block that opens on line `i`: (line index, trimmed text
/// of the line that opens each one).
fn children(lines: &[String], i: usize) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    for (k, l) in lines.iter().enumerate().skip(i) {
        let code = l.split("//").next().unwrap_or("");
        if depth == 1 && k > i && code.contains('{') {
            out.push((k, code.trim().to_string()));
        }
        depth += code.matches('{').count() as i32 - code.matches('}').count() as i32;
        if depth <= 0 && k > i {
            break;
        }
    }
    out
}

#[test]
fn scroll_areas_size_to_what_they_show() {
    // Slint sizes a ScrollView's / Flickable's content from its layout children and leaves
    // out `if` and `for` ones: with them straight inside, nothing scrolls (Settings did this).
    let mut problems = Vec::new();
    for (file, lines) in slint_files() {
        for (i, l) in lines.iter().enumerate() {
            let code = l.split("//").next().unwrap_or("");
            let opens = code.contains("ScrollView {") || code.contains("Flickable {");
            if !opens || code.trim_start().starts_with("export") {
                continue;
            }
            let sets_height = block(&lines, i).iter().any(|b| {
                let t = b.trim_start();
                t.starts_with("content-height:") || t.starts_with("viewport-height:")
            });
            let conditional = children(&lines, i).iter().any(|(_, c)| c.starts_with("if ") || c.starts_with("for "));
            if conditional && !sets_height && !excused(&lines, i) {
                problems.push(format!("  {file}:{}: {}", i + 1, l.trim()));
            }
        }
    }
    fail_if_any(
        problems,
        "These scroll areas have `if` / `for` children straight inside, which Slint leaves out of \
         the scrolled size, so they won't scroll. Put one VerticalLayout inside and the `if`s in it:",
    );
}

#[test]
fn dimmed_layers_use_the_backdrop() {
    // Backdrop (widgets.slint) is what makes a dialog, drawer or sheet close on Esc and on a
    // click outside, stop the wheel from scrolling the page behind, and keep Tab inside.
    let mut problems = Vec::new();
    for (file, lines) in slint_files() {
        if file == "widgets.slint" || file == "theme.slint" {
            continue;
        }
        for (i, l) in lines.iter().enumerate() {
            if l.contains("Theme.scrim") && !excused(&lines, i) {
                problems.push(format!("  {file}:{}: {}", i + 1, l.trim()));
            }
        }
    }
    fail_if_any(
        problems,
        "A dimmed layer over the page: use `Backdrop` (or `Dialog` / `Sheet`, built on it) from \
         widgets.slint instead of drawing Theme.scrim yourself:",
    );
}

#[test]
fn dialogs_you_type_in_ask_before_throwing_it_away() {
    let mut problems = Vec::new();
    for (file, lines) in slint_files() {
        for (i, l) in lines.iter().enumerate() {
            let t = l.trim_start();
            let component = t.starts_with("export component ") || t.starts_with("component ");
            let opens = (component && t.contains("inherits Dialog {")) || t.ends_with(": Dialog {") || t == "Dialog {";
            if !opens {
                continue;
            }
            let b = block(&lines, i);
            let types =
                b.iter().any(|x| x.contains("LineEdit {") || x.contains("TextEdit {") || x.contains("TextInput {"));
            let guarded = b.iter().any(|x| x.contains("dirty:") || x.contains("outside-closes"));
            if types && !guarded && !excused(&lines, i) {
                problems.push(format!("  {file}:{}: {}", i + 1, l.trim()));
            }
        }
    }
    fail_if_any(
        problems,
        "A dialog with a text field can lose what was typed on Esc, × or a click outside. If \
         it's saved with a button, give it `dirty: <changed since it opened>;` (closing then \
         asks \"Discard your changes?\"); if what's typed applies as it's typed, say so just \
         above the component: `// ui-rules: …`:",
    );
}

#[test]
fn no_number_box_that_the_wheel_changes() {
    // std's SpinBox changes its number under the mouse wheel even when it isn't focused, so
    // scrolling a page past it changes the setting.
    let mut problems = Vec::new();
    for (file, lines) in slint_files() {
        for (i, l) in lines.iter().enumerate() {
            let code = l.split("//").next().unwrap_or("");
            if code.contains("SpinBox {") && !excused(&lines, i) {
                problems.push(format!("  {file}:{}: {}", i + 1, l.trim()));
            }
        }
    }
    fail_if_any(problems, "Use a ComboBox (or ChoiceChips) instead of a SpinBox:");
}

#[test]
fn shared_controls_arent_copied() {
    // A page's own Hint, Toast or text box drifts from the shared one (sizes, Esc, hover).
    let widgets = fs::read_to_string(ui_dir().join("widgets.slint")).unwrap();
    let shared: Vec<String> = widgets
        .lines()
        .filter_map(|l| l.strip_prefix("export component "))
        .filter_map(|l| l.split_whitespace().next())
        .map(str::to_string)
        .collect();
    let mut problems = Vec::new();
    for (file, lines) in slint_files() {
        if file == "widgets.slint" {
            continue;
        }
        for (i, l) in lines.iter().enumerate() {
            let t = l.trim_start();
            let Some(rest) = t.strip_prefix("export component ").or_else(|| t.strip_prefix("component ")) else {
                continue;
            };
            let name = rest.split_whitespace().next().unwrap_or("");
            let base = rest.split("inherits").nth(1).map(str::trim).unwrap_or("");
            let copies = shared.iter().any(|s| s == name);
            let own_toast = name.ends_with("Toast") && !base.starts_with("Toast");
            if (copies || own_toast) && !excused(&lines, i) {
                problems.push(format!("  {file}:{}: {}", i + 1, l.trim()));
            }
        }
    }
    fail_if_any(
        problems,
        "These copy a control widgets.slint already has (a toast must inherit `Toast`). Import \
         the shared one, or give it what it lacks there:",
    );
}

#[test]
fn picked_menu_rows_say_so() {
    // A ✓ drawn as the row's icon is only a picture; `checked:` also tells a screen reader.
    let mut problems = Vec::new();
    for (file, lines) in slint_files() {
        for (i, l) in lines.iter().enumerate() {
            let t = l.trim_start();
            if t.starts_with("icon:") && t.contains("E73E}\" : \"\"") && !excused(&lines, i) {
                problems.push(format!("  {file}:{}: {}", i + 1, l.trim()));
            }
        }
    }
    fail_if_any(problems, "Mark the picked MenuItem with `checked: <condition>;` instead of a ✓ icon:");
}

#[test]
fn groups_of_choices_move_with_the_arrow_keys() {
    // Tabs, chips and section lists are one Tab stop with the arrow keys between them
    // (KeyPress `roving`); menus move with ↑ ↓ (`arrows`). Pin it in the shared controls.
    let widgets = fs::read_to_string(ui_dir().join("widgets.slint")).unwrap();
    let lines: Vec<String> = widgets.lines().map(str::to_string).collect();
    let mut problems = Vec::new();
    for name in ["Chip", "FilterTab", "NavItem"] {
        let i = lines.iter().position(|l| l.starts_with(&format!("export component {name} "))).unwrap();
        if !block(&lines, i).iter().any(|l| l.trim() == "roving: true;" || l.trim() == "roving: root.roving;") {
            problems.push(format!("  widgets.slint:{}: {name}", i + 1));
        }
    }
    let i = lines.iter().position(|l| l.starts_with("export component MenuItem ")).unwrap();
    if !block(&lines, i).iter().any(|l| l.trim() == "arrows: true;") {
        problems.push(format!("  widgets.slint:{}: MenuItem", i + 1));
    }
    fail_if_any(problems, "These lost their arrow keys (KeyPress `roving` / `arrows`):");
}

#[test]
fn spacing_stays_on_the_scale() {
    // One 4px scale (theme.slint `Space`, docs/DESIGN.md → Spacing): 0, 2, 4, 6, then
    // multiples of 4 up to 48. Odd values (5, 7, 10, 14, 22…) are how screens drift apart.
    let ok = |n: u32| n <= 2 || n == 4 || n == 6 || (n % 4 == 0 && n <= 48);
    let mut problems = Vec::new();
    for (file, lines) in slint_files() {
        for (i, l) in lines.iter().enumerate() {
            let code = l.split("//").next().unwrap_or("").trim();
            let Some((key, value)) = code.split_once(':') else { continue };
            let key = key.trim();
            if !["spacing", "padding", "padding-left", "padding-right", "padding-top", "padding-bottom"].contains(&key)
            {
                continue;
            }
            // Only literal lengths (alone or in `cond ? a : b`); sums and max() are worked out.
            if value.contains('(') || value.contains('+') || value.contains('/') || value.contains('*') {
                continue;
            }
            let off = value
                .split(|c: char| !(c.is_ascii_digit() || c == '.' || c == 'p' || c == 'x'))
                .filter_map(|w| w.strip_suffix("px"))
                .filter_map(|n| n.parse::<f32>().ok())
                .any(|n| n.fract() != 0.0 || !ok(n as u32));
            if off && !excused(&lines, i) {
                problems.push(format!("  {file}:{}: {}", i + 1, l.trim()));
            }
        }
    }
    fail_if_any(
        problems,
        "Spacing off the 4px scale (0, 2, 4, 6, 8, 12, 16, 20, 24, 28, 32…). Use a `Space` token \
         (hint 4, inline 8, title 12, item 16, group 32, inset 24) or the nearest step; an \
         alignment that must be exact gets `// ui-rules: <why>`:",
    );
}
