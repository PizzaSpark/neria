//! Spawning helpers for external tools.

use std::ffi::OsStr;
use std::process::{Child, Command, Stdio};

use crate::paths;

/// A `Command` with the bundled `bin/` first on PATH (so yt-dlp finds our
/// ffmpeg and deno), no console window on Windows, and stdin closed.
pub fn command(program: impl AsRef<OsStr>) -> Command {
    let mut cmd = Command::new(program);

    let mut dirs = vec![paths::bin_dir()];
    if let Some(path) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&path));
    }
    if let Ok(joined) = std::env::join_paths(dirs) {
        cmd.env("PATH", joined);
    }
    cmd.env("PYTHONIOENCODING", "utf-8");
    cmd.stdin(Stdio::null());

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// Kill a process and all of its children.
///
/// yt-dlp.exe is a PyInstaller bootloader that spawns the real interpreter,
/// which in turn spawns ffmpeg, so killing only the top process leaks the rest.
pub fn kill_tree(child: &mut Child) {
    #[cfg(windows)]
    {
        let _ = command("taskkill")
            .args(["/T", "/F", "/PID", &child.id().to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    let _ = child.kill();
}

/// Split a user-supplied argument string, honouring double and single quotes.
pub fn split_args(input: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut has_token = false;

    for c in input.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => cur.push(c),
            None if c == '"' || c == '\'' => {
                quote = Some(c);
                has_token = true;
            }
            None if c.is_whitespace() => {
                if has_token {
                    out.push(std::mem::take(&mut cur));
                    has_token = false;
                }
            }
            None => {
                cur.push(c);
                has_token = true;
            }
        }
    }
    if has_token {
        out.push(cur);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::split_args;

    #[test]
    fn splits_with_quotes() {
        assert_eq!(split_args(r#"--foo "a b" -x '' c"#), vec!["--foo", "a b", "-x", "", "c"]);
        assert!(split_args("   ").is_empty());
    }
}
