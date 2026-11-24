//! External tools (mock `design/tools.html`): the user's own command-line programs as Neria
//! actions, instead of a plugin system. A tool is a program, its words, and blanks Neria
//! fills in (`{file}`, `{out}`, `{dir}`, `{name}`, `{ext}`). Kept in `tools.json` next to the
//! exe; nothing comes with Neria, nothing is fetched, nothing can be imported from a link.
//!
//! The program is started directly with its words as separate pieces, never through a
//! shell, so a file name can't run anything. One run per file, in the queue (through a
//! one-off workflow, `workflow::Change::Tool`), with no console window; the row shows the
//! program's last line, and its whole output is kept for the report. A tool can run by itself
//! (a workflow for every download, a watched folder) only once it has run by hand and finished
//! (`tried`).

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

use crate::{clip, paths, proc};

/// What a tool takes.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum Takes {
    #[default]
    Videos,
    Sound,
    Both,
}

impl Takes {
    pub fn index(self) -> i32 {
        match self {
            Takes::Videos => 0,
            Takes::Sound => 1,
            Takes::Both => 2,
        }
    }

    pub fn from_index(i: i32) -> Self {
        match i {
            1 => Takes::Sound,
            2 => Takes::Both,
            _ => Takes::Videos,
        }
    }

    /// Does it take this file?
    pub fn fits(self, file: &Path) -> bool {
        let sound = crate::library::is_audio_file(file);
        match self {
            Takes::Videos => !sound,
            Takes::Sound => sound,
            Takes::Both => true,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct Tool {
    pub id: u64,
    pub name: String,
    /// The program: a path, or a name found in Neria's `bin` or on PATH (`ffmpeg`).
    pub program: String,
    /// Its words, with blanks: `-i {file} -o {out} -s 2`.
    pub words: String,
    /// It makes a new file ending in this (`mp4`); empty: it makes nothing.
    pub makes: String,
    pub takes: Takes,
    /// Has run by hand and finished: may run by itself in a workflow.
    pub tried: bool,
}

/// The blanks, in the order the editor offers them.
pub const BLANKS: &[&str] = &["{file}", "{out}", "{dir}", "{name}", "{ext}"];

impl Tool {
    pub fn makes_file(&self) -> bool {
        !self.makes.trim().is_empty()
    }

    /// The new file's ending, without the dot.
    pub fn out_ext(&self) -> String {
        self.makes.trim().trim_start_matches('.').to_lowercase()
    }

    /// "Takes videos · makes .mp4", "Takes sound files · makes nothing".
    pub fn summary(&self) -> String {
        let takes = match self.takes {
            Takes::Videos => "Takes videos",
            Takes::Sound => "Takes sound files",
            Takes::Both => "Takes videos and sound files",
        };
        if self.makes_file() {
            format!("{takes} · makes .{}", self.out_ext())
        } else {
            format!("{takes} · makes nothing")
        }
    }

    /// The program's own name ("realesrgan-ncnn-vulkan.exe").
    pub fn program_name(&self) -> String {
        Path::new(self.program.trim())
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.program.clone())
    }

    /// What's missing before it can be saved.
    pub fn problem(&self) -> Option<String> {
        if self.name.trim().is_empty() {
            return Some("Give it a name.".into());
        }
        if self.program.trim().is_empty() {
            return Some("Pick the program.".into());
        }
        if find_program(&self.program).is_none() {
            return Some(format!("Can't find {}. Pick it with Change….", self.program.trim()));
        }
        if !self.words.contains("{file}") && !self.words.contains("{dir}") && !self.words.contains("{name}") {
            return Some("Its words need {file}, so the program knows which file.".into());
        }
        if self.makes_file() {
            let ext = self.out_ext();
            if ext.is_empty() || !ext.chars().all(|c| c.is_ascii_alphanumeric()) {
                return Some("Type the new file's ending, like mp4.".into());
            }
        }
        None
    }
}

/// Where the program is: a path that exists, else Neria's `bin`, else PATH (`.exe` added
/// when it has no ending).
pub fn find_program(program: &str) -> Option<PathBuf> {
    let p = program.trim().trim_matches('"');
    if p.is_empty() {
        return None;
    }
    let path = PathBuf::from(p);
    if path.is_file() {
        return Some(path);
    }
    if path.components().count() > 1 {
        return None;
    }
    let names: Vec<String> =
        if path.extension().is_some() { vec![p.to_string()] } else { vec![format!("{p}.exe"), p.to_string()] };
    let mut dirs = vec![paths::bin_dir()];
    if let Some(var) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&var));
    }
    for d in dirs {
        for n in &names {
            let c = d.join(n);
            if c.is_file() {
                return Some(c);
            }
        }
    }
    None
}

/// A whole command line pasted into Program: the program and the rest of its words.
pub fn split_pasted(line: &str) -> Option<(String, String)> {
    let t = line.trim();
    if t.is_empty() || PathBuf::from(t.trim_matches('"')).is_file() {
        return None;
    }
    let parts = proc::split_args(t);
    if parts.len() < 2 {
        return None;
    }
    let program = parts[0].clone();
    let rest = t.strip_prefix('"').map_or_else(
        || t[t.find(char::is_whitespace).unwrap_or(t.len())..].trim().to_string(),
        |r| r[r.find('"').map_or(r.len(), |i| i + 1)..].trim().to_string(),
    );
    Some((program, rest))
}

/// The new file a run makes: next to `file`, "clip (Upscale 2×).mp4", never an existing one.
pub fn out_path(tool: &Tool, file: &Path) -> PathBuf {
    let dir = file.parent().unwrap_or(Path::new("."));
    let stem = file.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let tag = crate::music::sanitize(tool.name.trim());
    let ext = if tool.makes_file() {
        tool.out_ext()
    } else {
        file.extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_default()
    };
    clip::unique_path(dir, &format!("{stem} ({tag})"), &ext)
}

/// The program and its words with the blanks filled in for `file` (and `out`).
pub fn fill(tool: &Tool, file: &Path, out: &Path) -> (PathBuf, Vec<String>) {
    let program = find_program(&tool.program).unwrap_or_else(|| PathBuf::from(tool.program.trim()));
    let dir = file.parent().map(|d| d.display().to_string()).unwrap_or_default();
    let name = file.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let ext = file.extension().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let args = proc::split_args(&tool.words)
        .into_iter()
        .map(|w| {
            w.replace("{file}", &file.display().to_string())
                .replace("{out}", &out.display().to_string())
                .replace("{dir}", &dir)
                .replace("{name}", &name)
                .replace("{ext}", &ext)
        })
        .collect();
    (program, args)
}

/// The command as a person would type it, pieces with spaces quoted (the editor's It'll
/// run, the report).
pub fn shown(program: &Path, args: &[String]) -> String {
    let q = |s: &str| if s.is_empty() || s.contains([' ', '\t', '&', '(', ')']) { format!("\"{s}\"") } else { s.to_string() };
    let mut out = q(&program.display().to_string());
    for a in args {
        out.push(' ');
        out.push_str(&q(a));
    }
    out
}

/// The command for a sample file, for the editor.
pub fn sample(tool: &Tool) -> String {
    let file = dirs::video_dir().unwrap_or_else(|| PathBuf::from(r"C:\Videos")).join("clip.mp4");
    let file = if tool.takes == Takes::Sound { file.with_extension("mp3") } else { file };
    let out = {
        let stem = file.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let ext = if tool.makes_file() { tool.out_ext() } else { "mp4".into() };
        file.with_file_name(format!("{stem} ({}).{ext}", tool.name.trim()))
    };
    let (program, args) = fill(tool, &file, &out);
    shown(&program, &args)
}

/// "46.7%" anywhere in a line: the last such number, 0..1.
pub fn percent(line: &str) -> Option<f32> {
    let b = line.as_bytes();
    let mut found = None;
    for (i, &c) in b.iter().enumerate() {
        if c != b'%' {
            continue;
        }
        let mut j = i;
        while j > 0 && (b[j - 1].is_ascii_digit() || b[j - 1] == b'.') {
            j -= 1;
        }
        if j < i
            && let Ok(v) = line[j..i].parse::<f32>()
            && (0.0..=100.0).contains(&v)
        {
            found = Some(v / 100.0);
        }
    }
    found
}

/// What a run did.
pub struct Ran {
    /// The new file (a tool that makes one).
    pub out: Option<PathBuf>,
    /// The command as run, and everything it printed (for the report).
    pub command: String,
    pub output: Vec<String>,
}

/// A failed run: what went wrong, with the command and its output for the report.
#[derive(Debug)]
pub struct Failed {
    pub message: String,
    pub command: String,
    pub output: Vec<String>,
}

impl std::fmt::Display for Failed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Failed {}

/// A tool that makes nothing and is still running after this long (an editor it opened) is
/// left running, and counts as done.
const LEAVE_AFTER: Duration = Duration::from_secs(10);
/// Lines kept for the report.
const KEEP: usize = 400;

/// Run `tool` on `file`, making `out` (a tool that makes a file). `on` gets each line it
/// prints, and how far along it says it is when a line has a percentage.
pub fn run(
    tool: &Tool,
    file: &Path,
    out: &Path,
    cancel: &AtomicBool,
    mut on: impl FnMut(&str, Option<f32>),
) -> Result<Ran> {
    let (program, args) = fill(tool, file, out);
    let command = shown(&program, &args);
    let fail = |message: String, output: Vec<String>| -> anyhow::Error {
        anyhow::Error::new(Failed { message, command: command.clone(), output })
    };
    if find_program(&tool.program).is_none() {
        return Err(fail(format!("can't find {}", tool.program.trim()), Vec::new()));
    }
    let mut cmd = proc::command(&program);
    cmd.args(&args).stdout(Stdio::piped()).stderr(Stdio::piped());
    if let Some(dir) = file.parent() {
        cmd.current_dir(dir);
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return Err(fail(format!("couldn't start {}: {e}", tool.program_name()), Vec::new())),
    };
    // Both streams, line by line (a `\r` progress line counts as a line too).
    let (tx, rx) = mpsc::channel::<String>();
    for stream in [child.stdout.take().map(|s| Box::new(s) as Box<dyn Read + Send>), child.stderr.take().map(|s| Box::new(s) as Box<dyn Read + Send>)]
        .into_iter()
        .flatten()
    {
        let tx = tx.clone();
        std::thread::spawn(move || {
            let mut r = BufReader::new(stream);
            let mut buf = Vec::new();
            loop {
                buf.clear();
                let mut byte = [0u8; 1];
                let mut got = false;
                while let Ok(1) = r.read(&mut byte) {
                    got = true;
                    if byte[0] == b'\n' || byte[0] == b'\r' {
                        break;
                    }
                    buf.push(byte[0]);
                }
                if !got {
                    break;
                }
                let line = String::from_utf8_lossy(&buf).trim().to_string();
                if !line.is_empty() && tx.send(line).is_err() {
                    break;
                }
            }
            let _ = r.fill_buf();
        });
    }
    drop(tx);
    let output: Arc<Mutex<VecDeque<String>>> = Arc::default();
    let started = Instant::now();
    let status = loop {
        while let Ok(line) = rx.try_recv() {
            on(&line, percent(&line));
            let mut o = output.lock().unwrap();
            o.push_back(line);
            if o.len() > KEEP {
                o.pop_front();
            }
        }
        if cancel.load(Ordering::SeqCst) {
            proc::kill_tree(&mut child);
            let _ = child.wait();
            if let Ok(meta) = std::fs::metadata(out)
                && tool.makes_file()
                && meta.is_file()
            {
                let _ = std::fs::remove_file(out);
            }
            bail!("Cancelled");
        }
        match child.try_wait() {
            Ok(Some(st)) => break Some(st),
            Ok(None) => {}
            Err(e) => return Err(fail(format!("lost track of {}: {e}", tool.program_name()), Vec::new())),
        }
        if !tool.makes_file() && started.elapsed() >= LEAVE_AFTER {
            // It opened something and stays open: leave it be.
            break None;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    // What's left of its output.
    std::thread::sleep(Duration::from_millis(50));
    while let Ok(line) = rx.try_recv() {
        on(&line, percent(&line));
        output.lock().unwrap().push_back(line);
    }
    let output: Vec<String> = output.lock().unwrap().iter().cloned().collect();
    let last = output.last().cloned().unwrap_or_default();
    if let Some(st) = status
        && !st.success()
    {
        // Small codes as they are; Windows' own (0xC0000005…) in hex, as people look them up.
        let code = match st.code() {
            Some(c) if (0..=255).contains(&c) => format!("code {c}"),
            Some(c) => format!("error {:#010X}", c as u32),
            None => "it was stopped".to_string(),
        };
        let msg = if last.is_empty() {
            format!("{} quit ({code})", tool.name)
        } else {
            format!("{} quit ({code}): {last}", tool.name)
        };
        return Err(fail(msg, output));
    }
    let made = if tool.makes_file() {
        match std::fs::metadata(out) {
            Ok(m) if m.is_file() && m.len() > 0 => Some(out.to_path_buf()),
            _ => {
                let _ = std::fs::remove_file(out);
                return Err(fail(
                    format!("{} finished but made no file (its words need {{out}} where the new file goes)", tool.name),
                    output,
                ));
            }
        }
    } else {
        None
    };
    mark_tried(tool.id);
    Ok(Ran { out: made, command, output })
}

/// The report of a failed run: the command and what it printed.
pub fn report(e: &anyhow::Error) -> Option<String> {
    let f = e.downcast_ref::<Failed>()?;
    let mut s = format!("{}\n\nCommand:\n{}\n\nIt printed:\n", f.message, f.command);
    if f.output.is_empty() {
        s.push_str("(nothing)\n");
    }
    for l in &f.output {
        s.push_str(l);
        s.push('\n');
    }
    Some(s)
}

// ---------------------------------------------------------------------------------------
// The list

#[derive(Serialize, Deserialize, Default)]
struct Store {
    next_id: u64,
    items: Vec<Tool>,
}

static LOCK: Mutex<()> = Mutex::new(());

fn file() -> PathBuf {
    paths::app_dir().join("tools.json")
}

fn load_store() -> Store {
    std::fs::read(file()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn store(s: &Store) {
    if let Ok(json) = serde_json::to_vec_pretty(s) {
        let tmp = file().with_extension("json.tmp");
        if std::fs::write(&tmp, json).is_ok() {
            let _ = std::fs::rename(&tmp, file());
        }
    }
}

fn edit<T>(f: impl FnOnce(&mut Store) -> T) -> T {
    let _guard = LOCK.lock().unwrap();
    let mut s = load_store();
    let out = f(&mut s);
    store(&s);
    out
}

pub fn list() -> Vec<Tool> {
    let _guard = LOCK.lock().unwrap();
    load_store().items
}

pub fn get(id: u64) -> Option<Tool> {
    list().into_iter().find(|t| t.id == id)
}

/// Save a new one (id 0) or changes to one; returns its id. Changing what it runs means
/// trying it again before it runs by itself.
pub fn save(mut tool: Tool) -> u64 {
    edit(|s| {
        if let Some(old) = s.items.iter_mut().find(|t| t.id == tool.id && tool.id != 0) {
            let same = old.program.trim() == tool.program.trim()
                && old.words.trim() == tool.words.trim()
                && old.makes == tool.makes;
            tool.tried = old.tried && same;
            *old = tool.clone();
            return tool.id;
        }
        s.next_id = s.next_id.max(s.items.iter().map(|t| t.id).max().unwrap_or(0) + 1).max(1);
        tool.id = s.next_id;
        tool.tried = false;
        s.next_id += 1;
        s.items.push(tool.clone());
        tool.id
    })
}

pub fn delete(id: u64) {
    edit(|s| s.items.retain(|t| t.id != id));
}

fn mark_tried(id: u64) {
    if id == 0 {
        return;
    }
    edit(|s| {
        if let Some(t) = s.items.iter_mut().find(|t| t.id == id) {
            t.tried = true;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn upscale() -> Tool {
        Tool {
            id: 1,
            name: "Upscale 2×".into(),
            program: r"D:\Tools\up.exe".into(),
            words: "-i {file} -o {out} -s 2".into(),
            makes: "mp4".into(),
            ..Tool::default()
        }
    }

    #[test]
    fn blanks_are_filled_as_whole_pieces() {
        let file = Path::new(r"D:\Vids\a&b clip.mp4");
        let out = out_path(&upscale(), file);
        assert_eq!(out, PathBuf::from(r"D:\Vids\a&b clip (Upscale 2×).mp4"));
        let (_, args) = fill(&upscale(), file, &out);
        assert_eq!(args, vec!["-i", r"D:\Vids\a&b clip.mp4", "-o", r"D:\Vids\a&b clip (Upscale 2×).mp4", "-s", "2"]);
        let t = Tool { words: "--input={file} --name {name}.{ext} --in {dir}".into(), ..upscale() };
        let (_, args) = fill(&t, file, &out);
        assert_eq!(args, vec![r"--input=D:\Vids\a&b clip.mp4", "--name", "a&b clip.mp4", "--in", r"D:\Vids"]);
    }

    #[test]
    fn shown_quotes_what_needs_it() {
        let s = shown(Path::new(r"C:\Program Files\x.exe"), &["-i".into(), r"D:\a b.mp4".into(), "-q".into()]);
        assert_eq!(s, r#""C:\Program Files\x.exe" -i "D:\a b.mp4" -q"#);
    }

    #[test]
    fn finds_percentages() {
        assert_eq!(percent("frame 812/1740 · 46.7%"), Some(0.467));
        assert_eq!(percent("Encoding: task 1 of 1, 12.50 % (40 fps)"), None);
        assert_eq!(percent("[download]  99.9% of 10MiB"), Some(0.999));
        assert_eq!(percent("no number here"), None);
        assert_eq!(percent("150%"), None);
    }

    #[test]
    fn a_pasted_line_splits_once() {
        assert_eq!(
            split_pasted(r#""C:\Program Files\HandBrake\HandBrakeCLI.exe" -i {file} -o {out}"#),
            Some((r"C:\Program Files\HandBrake\HandBrakeCLI.exe".into(), "-i {file} -o {out}".into()))
        );
        assert_eq!(split_pasted("ffmpeg -i {file} {out}"), Some(("ffmpeg".into(), "-i {file} {out}".into())));
        assert_eq!(split_pasted("ffmpeg"), None);
    }

    #[test]
    fn problems_in_plain_words() {
        let t = Tool { name: "".into(), ..upscale() };
        assert_eq!(t.problem().as_deref(), Some("Give it a name."));
        let t = Tool { program: "no-such-program-here".into(), ..upscale() };
        assert!(t.problem().unwrap().starts_with("Can't find"));
        assert!(Takes::Videos.fits(Path::new("a.mkv")));
        assert!(!Takes::Videos.fits(Path::new("a.mp3")));
    }
}
