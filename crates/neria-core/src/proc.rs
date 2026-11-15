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

/// Make `child` (and whatever it starts from now on: yt-dlp's ffmpeg) end when Neria does,
/// even when Neria crashes or is killed. For what never ends by itself (recording a live
/// stream, the replay buffer): an orphan would record, and fill the drive, forever.
///
/// Windows: every such process goes in one job object that ends its processes when its
/// last handle closes, which is when Neria's process goes away. Elsewhere: nothing.
pub fn end_with_neria(child: &Child) {
    #[cfg(windows)]
    imp::assign(child);
    #[cfg(not(windows))]
    let _ = child;
}

#[cfg(windows)]
mod imp {
    use std::ffi::c_void;
    use std::os::windows::io::AsRawHandle;
    use std::process::Child;
    use std::sync::OnceLock;

    #[repr(C)]
    #[derive(Default)]
    struct BasicLimits {
        per_process_user_time: i64,
        per_job_user_time: i64,
        limit_flags: u32,
        min_working_set: usize,
        max_working_set: usize,
        active_process_limit: u32,
        affinity: usize,
        priority_class: u32,
        scheduling_class: u32,
    }

    #[repr(C)]
    #[derive(Default)]
    struct ExtendedLimits {
        basic: BasicLimits,
        io: [u64; 6],
        process_memory_limit: usize,
        job_memory_limit: usize,
        peak_process_memory: usize,
        peak_job_memory: usize,
    }

    const JOB_OBJECT_EXTENDED_LIMIT_INFORMATION: i32 = 9;
    const JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE: u32 = 0x2000;

    unsafe extern "system" {
        fn CreateJobObjectW(attributes: *const c_void, name: *const u16) -> *mut c_void;
        fn SetInformationJobObject(job: *mut c_void, class: i32, info: *const c_void, len: u32) -> i32;
        fn AssignProcessToJobObject(job: *mut c_void, process: *mut c_void) -> i32;
    }

    /// The job, made once and never closed (its handle lives as long as Neria).
    struct Job(*mut c_void);
    // SAFETY: a job handle can be used from any thread.
    unsafe impl Send for Job {}
    unsafe impl Sync for Job {}

    fn job() -> Option<&'static Job> {
        static JOB: OnceLock<Option<Job>> = OnceLock::new();
        JOB.get_or_init(|| {
            // SAFETY: plain Win32 calls; `info` outlives the call that reads it.
            unsafe {
                let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if job.is_null() {
                    return None;
                }
                let mut info = ExtendedLimits::default();
                info.basic.limit_flags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                let ok = SetInformationJobObject(
                    job,
                    JOB_OBJECT_EXTENDED_LIMIT_INFORMATION,
                    &info as *const ExtendedLimits as *const c_void,
                    std::mem::size_of::<ExtendedLimits>() as u32,
                );
                (ok != 0).then_some(Job(job))
            }
        })
        .as_ref()
    }

    pub fn assign(child: &Child) {
        if let Some(job) = job() {
            // SAFETY: the child's handle is valid while `child` is.
            unsafe {
                AssignProcessToJobObject(job.0, child.as_raw_handle());
            }
        }
    }
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
