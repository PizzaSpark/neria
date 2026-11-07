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
    if i == 0 {
        format!("{v:.0} {}", UNITS[i])
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}

pub fn duration(secs: f64) -> String {
    let s = secs.max(0.0).round() as u64;
    let (h, m, s) = (s / 3600, (s % 3600) / 60, s % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
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
        _ => "Processing…",
    }
}

/// (status line, stats line) for a job row.
pub fn job_lines(s: &JobSnapshot) -> (String, String) {
    match s.state {
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
        JobState::Processing => (
            postprocessor(s.postprocessor.as_deref().unwrap_or("")).into(),
            String::new(),
        ),
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
            (name, size)
        }
        JobState::Failed => (
            s.error.clone().unwrap_or_else(|| "Download failed".into()),
            String::new(),
        ),
        JobState::Cancelled => ("Cancelled".into(), String::new()),
    }
}
