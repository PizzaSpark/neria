//! Human-friendly formatting shared by every front-end.

use crate::queue::{JobSnapshot, JobState};

pub fn bytes(b: f64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = b.max(0.0);
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 { format!("{v:.0} {}", UNITS[i]) } else { format!("{v:.1} {}", UNITS[i]) }
}

/// A size the way upload limits count it (1 MB = 1,000,000 bytes), so "19.6 MB" sits under
/// Discord's "20 MB". `bytes` counts in 1024s, which reads as over the limit.
pub fn mb(b: f64) -> String {
    let m = b.max(0.0) / 1e6;
    if m >= 1000.0 {
        format!("{:.1} GB", m / 1000.0)
    } else if m >= 100.0 {
        format!("{m:.0} MB")
    } else if m >= 0.1 {
        format!("{m:.1} MB")
    } else {
        format!("{:.0} KB", b / 1e3)
    }
}

pub fn duration(secs: f64) -> String {
    let s = secs.max(0.0).round() as u64;
    let (h, m, s) = (s / 3600, (s % 3600) / 60, s % 60);
    if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m}:{s:02}") }
}

/// "3 items · 1 photo, 1 video, 1 GIF" (or "12 videos" when they're all one kind).
pub fn item_counts(items: &[crate::post::Item]) -> String {
    use crate::post::ItemKind::*;
    let count = |k| items.iter().filter(|i| i.kind == k).count();
    let word = |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
    let parts: Vec<String> =
        [(count(Photo), "photo", "photos"), (count(Video), "video", "videos"), (count(Gif), "GIF", "GIFs")]
            .into_iter()
            .filter(|(n, _, _)| *n > 0)
            .map(|(n, one, many)| word(n, one, many))
            .collect();
    match parts.as_slice() {
        [only] => only.clone(),
        _ => format!("{}  ·  {}", word(items.len(), "item", "items"), parts.join(", ")),
    }
}

pub fn postprocessor(name: &str) -> &'static str {
    match name {
        "Merger" => "Merging video and audio…",
        "ExtractAudio" => "Converting audio…",
        "EmbedThumbnail" => "Embedding thumbnail…",
        "FFmpegMetadata" | "Metadata" => "Writing metadata…",
        "EmbedSubtitle" => "Embedding subtitles…",
        "SponsorBlock" | "ModifyChapters" => "Removing sponsor segments…",
        "VideoRemuxer" | "VideoConvertor" => "Converting video…",
        "MoveFiles" => "Finishing up…",
        "Matching" => "Finding it on YouTube Music…",
        "Verifying" => "Checking the sound against the official preview…",
        "Retrying" => "Different recording, trying another version…",
        "Tagging" => "Adding cover, tags and lyrics…",
        "Lyrics" => "Looking for lyrics…",
        "MakingGif" => "Turning it into a GIF…",
        "Sound" => "Changing the sound…",
        "Picture" => "Cropping and turning the picture…",
        "Joining" => "Joining the chapters…",
        "Transcribing" => "Writing the transcript…",
        "SavingChat" => "Saving the chat…",
        "Chat" => "Putting the chat in…",
        "UpdatingYtdlp" => "The site changed. Updating the downloader, then trying again…",
        _ => "Processing…",
    }
}

/// A recording's row: the tape (stream start → now, as fractions) and its lines, in plain
/// words. `compact` is the 400 px row (shorter lines, no line under the tape).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LiveRow {
    pub status: String,
    pub stats: String,
    /// Under the tape: where it begins, what's going on, "now".
    pub left: String,
    pub middle: String,
    /// `middle` is about something missed (drawn in the warning colour).
    pub middle_bad: bool,
    pub right: String,
    /// The tape: before you joined (hatched) up to `joined`, recorded up to `done`.
    pub joined: f32,
    pub done: f32,
    /// The part before (fetched next to it): in from the start up to here, 0..1; solid
    /// once all of it is in (`early_in`), fainter while it comes.
    pub early: f32,
    pub early_in: bool,
    pub holes: Vec<f32>,
    pub catching_up: bool,
    /// Shows the tape (running, not waiting or saving).
    pub tape: bool,
    pub can_clip: bool,
    /// "Start clip" was pressed: the row offers "End clip".
    pub clipping: bool,
}

/// "missed 0:42 at 0:31:05" (where in the recording), "missed a moment at 0:22", or for
/// several "missed 3 bits, 2:10 in all".
fn missed_words(holes: &[crate::record::Hole], joined: f64) -> String {
    match holes {
        [] => String::new(),
        [h] if h.about => format!("missed a moment at {}", duration(h.at - joined)),
        [h] => format!("missed {} at {}", duration(h.len), duration(h.at - joined)),
        many if many.iter().any(|h| h.about) => format!("missed {} bits", many.len()),
        many => format!("missed {} bits, {} in all", many.len(), duration(many.iter().map(|h| h.len).sum())),
    }
}

pub fn live_row(s: &JobSnapshot, now: f64, compact: bool) -> Option<LiveRow> {
    use crate::record::Phase;
    if !s.recording {
        return None;
    }
    let st = s.live.clone().unwrap_or_default();
    let joined = st.joined.unwrap_or(0.0);
    let missed = missed_words(&st.holes, joined);
    let mut r = LiveRow { right: "now".into(), can_clip: st.can_clip, ..LiveRow::default() };
    let size = |b: u64| if b > 0 { bytes(b as f64) } else { String::new() };
    let join = |parts: &[String]| parts.iter().filter(|p| !p.is_empty()).cloned().collect::<Vec<_>>().join("  ·  ");
    match s.state {
        JobState::Queued => {
            r.status = "Waiting to start…".into();
            return Some(r);
        }
        JobState::Done => {
            r.status = format!("{} recorded", duration(st.recorded));
            r.stats = join(&[s.downloaded.map(bytes).unwrap_or_default(), s.note.clone().unwrap_or_default()]);
            r.middle = missed;
            r.middle_bad = !r.middle.is_empty();
            return Some(r);
        }
        JobState::Failed | JobState::Cancelled | JobState::Paused => return None,
        _ => {}
    }
    let height = st.height.filter(|h| *h > 0).map(|h| format!("{h}p")).unwrap_or_default();
    let speed = if st.speed > 0.0 { format!("{}/s", bytes(st.speed)) } else { Default::default() };
    match st.phase {
        Phase::Waiting => {
            r.status = match st.starts_at.map(|t| t - now) {
                Some(d) if d >= 1.0 => format!("Starts in {}", duration(d)),
                Some(_) => "Should start any moment".into(),
                None => "Waiting for it to start".into(),
            };
            r.stats = if compact {
                "waiting".into()
            } else {
                "records from the first second  ·  checks every minute if it's late".into()
            };
            return Some(r);
        }
        Phase::Connecting => {
            r.status = "Connecting…".into();
            return Some(r);
        }
        Phase::Saving => {
            r.status = "Saving…".into();
            r.stats = join(&[format!("{} recorded", duration(st.recorded)), size(st.bytes)]);
            return Some(r);
        }
        _ => {}
    }
    // The tape: stream start → now. A long stream joined late shows a little before the
    // join instead (else it'd be all hatching and a sliver).
    let len = st.stream_len(now).max(st.position).max(1.0);
    let span = (st.position - joined).max(1.0);
    // Getting the part before: all of it, from the stream's start.
    let earlier = st.earlier.is_some() || st.earlier_done;
    let from = if joined > span * 2.0 && !earlier { (joined - span * 0.25).max(0.0) } else { 0.0 };
    let frac = |t: f64| (((t - from) / (len - from)).clamp(0.0, 1.0)) as f32;
    r.tape = true;
    r.joined = frac(joined);
    r.done = frac(st.position);
    if let Some((got, _)) = st.earlier {
        r.early = frac(got.min(joined));
    } else if st.earlier_done {
        (r.early, r.early_in) = (r.joined, true);
    }
    r.holes = st.holes.iter().map(|h| frac(h.at)).collect();
    let ago = |t: f64| duration((len - t).max(0.0));
    r.left = if joined < 1.0 || earlier {
        format!("started {} ago", ago(0.0))
    } else {
        format!("you joined {} ago", ago(joined))
    };
    match st.phase {
        Phase::CatchingUp => {
            r.catching_up = true;
            let behind = (len - st.position).max(0.0);
            let eta = behind / (crate::record::CATCH_UP_SPEED - 1.0);
            let live_in = format!("live in ~{}", crate::record::age_words(eta));
            r.middle = format!("catching up: {} of {}  ·  {live_in}", duration(st.position), duration(len));
            if compact {
                r.status = format!("{} of {}", duration(st.position), duration(len));
                r.stats = live_in;
                r.middle.clear();
            } else {
                r.status = format!("{} recorded", duration(st.recorded));
                r.stats = join(&[size(st.bytes), height, speed]);
            }
        }
        Phase::Reconnecting => {
            r.status = "Reconnecting…".into();
            r.stats = join(&[format!("{} so far", duration(st.recorded)), size(st.bytes)]);
            r.middle = missed;
            r.middle_bad = !r.middle.is_empty();
        }
        _ => {
            if compact {
                r.status = duration(st.recorded);
                r.stats = size(st.bytes);
            } else {
                r.status = format!("{} recorded", duration(st.recorded));
                r.stats = join(&[size(st.bytes), height, speed]);
            }
            if let Some(from) = s.clip_from {
                let so_far = duration((st.recorded - from).max(0.0));
                r.middle = if compact {
                    format!("clip: {so_far}")
                } else {
                    format!("clipping from {}  ·  {so_far} so far", duration(from))
                };
                r.middle_bad = false;
                r.clipping = true;
            } else if st.rewind_failed && missed.is_empty() {
                r.middle = if compact { "from now".into() } else { "couldn't go back to the start".into() };
            } else {
                // Compact: "missed 0:42" (where is in the wide row).
                r.middle = if compact { missed.split(" at ").next().unwrap_or("").to_string() } else { missed };
                r.middle_bad = !r.middle.is_empty();
            }
        }
    }
    Some(r)
}

/// (status line, stats line) for a job row.
pub fn job_lines(s: &JobSnapshot) -> (String, String) {
    match s.state {
        JobState::Queued if !s.starts.is_empty() => (format!("Starts at {}", s.starts), String::new()),
        JobState::Queued => ("Waiting in queue…".into(), String::new()),
        JobState::Downloading => {
            let status = if s.stages > 1 {
                format!("Downloading {}/{}", (s.stage + 1).min(s.stages), s.stages)
            } else {
                "Downloading".into()
            };
            let mut parts = Vec::new();
            match (s.downloaded, s.total) {
                (Some(d), Some(t)) if t > 0.0 => parts.push(format!("{} / {}", bytes(d), bytes(t))),
                (Some(d), _) => parts.push(bytes(d)),
                _ => {}
            }
            if let Some(sp) = s.speed {
                parts.push(format!("{}/s", bytes(sp)));
            }
            if let Some(eta) = s.eta {
                parts.push(format!("{} left", duration(eta)));
            }
            (status, parts.join("  ·  "))
        }
        JobState::Processing => (postprocessor(s.postprocessor.as_deref().unwrap_or("")).into(), String::new()),
        JobState::Done if s.postprocessor.as_deref() == Some("AlreadyHave") => {
            ("Already downloaded".into(), String::new())
        }
        JobState::Done if s.note.is_some() => (s.note.clone().unwrap_or_default(), String::new()),
        JobState::Done => {
            let name = s
                .file
                .as_ref()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "Saved".into());
            let size = s
                .file
                .as_ref()
                .and_then(|p| std::fs::metadata(p).ok())
                .map(|m| bytes(m.len() as f64))
                .unwrap_or_default();
            let mut parts = Vec::new();
            match s.verified {
                Some("none") => parts.push("no preview to check against".to_string()),
                Some("Spotify") => parts.push("✓ sounds like Spotify".into()),
                Some(src) => parts.push(format!("✓ matches the {src} preview")),
                None => {}
            }
            parts.extend(s.lyrics.clone());
            if !size.is_empty() {
                parts.push(size);
            }
            (name, parts.join("  ·  "))
        }
        JobState::Failed => (s.error.clone().unwrap_or_else(|| "Download failed".into()), String::new()),
        JobState::Cancelled => ("Cancelled".into(), String::new()),
        JobState::Paused => {
            let status = match (s.downloaded, s.total) {
                // The row's tag already says "Paused".
                (Some(d), Some(t)) if t > 0.0 => format!("{} of {} so far", bytes(d), bytes(t)),
                (Some(d), _) if d > 0.0 => format!("{} so far", bytes(d)),
                _ => "Resumes where it left off".into(),
            };
            (status, String::new())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::{Hole, LiveStats, Phase};

    fn rec(state: JobState, st: LiveStats) -> JobSnapshot {
        let mut s = JobSnapshot::sample(1, "Live", state);
        s.recording = true;
        s.live = Some(st);
        s
    }

    #[test]
    fn the_tape_shows_the_part_before_coming_in() {
        let now = 100_000.0;
        // On for 54 min, recorded from now for a minute, 12 min of the start in so far.
        let st = LiveStats {
            phase: Phase::Recording,
            started_at: Some(now - 3300.0),
            recorded: 60.0,
            position: 3290.0,
            joined: Some(3230.0),
            earlier: Some((720.0, 3230.0)),
            ..LiveStats::default()
        };
        let r = live_row(&rec(JobState::Downloading, st.clone()), now, false).unwrap();
        // The whole stream from its start, not a little before the join.
        assert!((r.joined - 3230.0 / 3300.0).abs() < 0.01, "{}", r.joined);
        assert!((r.early - 720.0 / 3300.0).abs() < 0.01, "{}", r.early);
        assert!(!r.early_in);
        assert!(r.left.starts_with("started"), "{}", r.left);
        // All in: solid up to the join.
        let done = LiveStats { earlier: None, earlier_done: true, ..st };
        let r = live_row(&rec(JobState::Downloading, done), now, false).unwrap();
        assert!(r.early_in && (r.early - r.joined).abs() < 1e-6);
    }

    #[test]
    fn a_recording_row_says_what_it_has() {
        let now = 100_000.0;
        // Joined an hour in, missed 42 s half an hour after joining.
        let st = LiveStats {
            phase: Phase::Recording,
            started_at: Some(now - 10_800.0),
            recorded: 3750.0,
            position: 10_790.0,
            joined: Some(7008.0),
            holes: vec![Hole { at: 7008.0 + 1865.0, len: 42.0, about: false }],
            bytes: 640_000_000,
            height: Some(720),
            ..LiveStats::default()
        };
        let r = live_row(&rec(JobState::Downloading, st.clone()), now, false).unwrap();
        assert_eq!(r.status, "1:02:30 recorded");
        assert_eq!(r.left, "you joined 1:03:12 ago");
        assert_eq!(r.middle, "missed 0:42 at 31:05");
        assert!(r.middle_bad && r.tape);
        assert!((r.joined - 0.65).abs() < 0.01, "{}", r.joined);
        assert!(r.holes[0] > r.joined && r.holes[0] < r.done);
        // On for 11 days, joined 10 min ago: the tape shows a little before the join, not
        // days of hatching and a sliver.
        let long = LiveStats {
            phase: Phase::Recording,
            started_at: Some(now - 950_400.0),
            recorded: 600.0,
            position: 950_390.0,
            joined: Some(949_790.0),
            ..LiveStats::default()
        };
        let r = live_row(&rec(JobState::Downloading, long), now, false).unwrap();
        assert!(r.joined > 0.15 && r.joined < 0.25, "{}", r.joined);
        assert_eq!(r.left, "you joined 10:10 ago");
        let c = live_row(&rec(JobState::Downloading, st), now, true).unwrap();
        assert_eq!((c.status.as_str(), c.middle.as_str()), ("1:02:30", "missed 0:42"));
    }

    #[test]
    fn catching_up_and_waiting() {
        let now = 100_000.0;
        let st = LiveStats {
            phase: Phase::CatchingUp,
            started_at: Some(now - 8040.0),
            recorded: 3070.0,
            position: 3070.0,
            joined: Some(0.0),
            ..LiveStats::default()
        };
        let r = live_row(&rec(JobState::Downloading, st.clone()), now, false).unwrap();
        assert_eq!(r.left, "started 2:14:00 ago");
        assert_eq!(r.middle, "catching up: 51:10 of 2:14:00  ·  live in ~17 min");
        assert!(r.catching_up && r.joined == 0.0);
        let c = live_row(&rec(JobState::Downloading, st), now, true).unwrap();
        assert_eq!(
            (c.status.as_str(), c.stats.as_str(), c.middle.as_str()),
            ("51:10 of 2:14:00", "live in ~17 min", "")
        );
        let wait = LiveStats { phase: Phase::Waiting, starts_at: Some(now + 2530.0), ..LiveStats::default() };
        let r = live_row(&rec(JobState::Downloading, wait), now, false).unwrap();
        assert_eq!(r.status, "Starts in 42:10");
        assert!(!r.tape);
    }

    #[test]
    fn a_saved_recording() {
        let st = LiveStats {
            recorded: 10_961.0,
            joined: Some(0.0),
            holes: vec![Hole { at: 4385.0, len: 42.0, about: false }],
            ..LiveStats::default()
        };
        let mut s = rec(JobState::Done, st);
        s.note = Some("the stream ended".into());
        s.downloaded = Some(1_400_000_000.0);
        let r = live_row(&s, 0.0, false).unwrap();
        assert_eq!(r.status, "3:02:41 recorded");
        assert!(r.stats.ends_with("the stream ended"), "{}", r.stats);
        assert_eq!(r.middle, "missed 0:42 at 1:13:05");
        // Not a recording: no live row.
        assert!(live_row(&JobSnapshot::sample(2, "x", JobState::Done), 0.0, false).is_none());
    }
}
