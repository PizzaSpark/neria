# Neria roadmap

Fast, portable, UX-first video/audio downloader. Rust + Slint, yt-dlp + ffmpeg (+ Deno) auto-fetched into `bin/`.

## Phase 1: Downloader ✅
- First-run auto-fetch of yt-dlp, FFmpeg, Deno; daily `yt-dlp -U`
- Paste link → instant preview (title, thumbnail, channel, length)
- Quality chips (Best / 1080p / 720p / 480p / Audio only), queue with live progress, cancel/retry
- Advanced drawer: container, H.264, audio format, embeds, SponsorBlock, playlists, cookies, rate limit, concurrency, filename template, raw yt-dlp args

## Phase 2: Clipper (local files) ✅
- `neria-player` crate: libmpv (auto-fetched, loaded at runtime) rendered into a GL texture shown by Slint
- Timeline with filmstrip + audio waveform, drag in/out handles, scrubbing, frame stepping,
  shortcuts (Space/K, I/O, J/L and arrows, `,` `.`, Home/End)
- Export: lossless (fast, keyframe-snapped) or re-encode (frame-exact H.264); audio files too
- Scissors button on finished downloads opens them in the clipper
- Lossless mode shows keyframe ticks, highlights in amber any extra footage before the in point
  (where the cut really starts), with a one-click "Snap to keyframe"
- Playback stops at the Out handle; play from there replays the selection from In; loop toggle
  (button / R) repeats the selection
- Audio controls (to do), two separate things so it's clear what ends up in the file:
  - **Preview volume**: a volume slider + mute for listening while you edit (mpv `volume`);
    never changes the exported clip
  - **Clip audio**, applied on export: keep as is / mute (no audio track) / adjust gain
    (± dB) / **normalize** loudness (ffmpeg `loudnorm`, EBU R128, presets like -14 LUFS for
    YouTube/TikTok/Discord and -16 for podcasts). Option to preview the effect live (mpv
    `af=loudnorm` / `volume`) before exporting
  - Fast · lossless can still be used: video stays stream-copied, only the audio is
    re-encoded when an audio change is picked (say so in the UI)
- Polish still to do: drag-and-drop files from Explorer

## Phase 3: Clip before download ✅
- "Clip first" on a link preview, or "Paste link" on the Clip page, streams the link in mpv
  (mpv's yt-dlp hook, 720p preview). Filmstrip from the site's storyboards (YouTube's
  hover-preview sprite sheets, cropped per frame); YouTube now answers direct ffmpeg reads of
  its streams with 403, so frames from a low-res stream are only a fallback for other sites
- Download clip sends only the selected range to the queue (`--download-sections`), in the
  chosen quality; Exact adds `--force-keyframes-at-cuts` for frame-exact cuts
- Polish still to do: waveform for links (needs the audio); live/very long streams untested

## Phase 4: Compressor ✅
- Fit a size (10 MB Discord / 25 / 50 / 100 / custom) or pick a quality (slider → CRF/CQ/QP)
- Auto resolution/fps from bits-per-pixel (e.g. 10 MB × 60 s → 720p30); manual caps; audio
  bitrate or remove; stereo downmix
- H.264 / H.265 / AV1; GPU encoders (NVENC / Quick Sync / AMF) detected by a real test encode,
  CPU fallback with two-pass for x264/x265
- Guaranteed fit: if the result overshoots, it re-encodes smaller (up to 3 tries)
- Compress button on finished downloads
- Polish still to do: before/after preview; batch compress; trim inside the compressor

## Phase 5: Spotify links (spotDL-style)
Spotify audio itself is DRM-protected, so Neria does what spotDL does:
read the track/album/playlist metadata from Spotify, find the matching song on
YouTube Music, download it with the bundled yt-dlp, and tag it with Spotify's metadata.

Status: ✅ core flow done (`spotify.rs`, `ytmusic.rs`, `music.rs`, desktop `spotify_ui.rs`).
Tested on Random Access Memories (13/13 matched at 1.00, whole album in 83 s) and All Out 80s.

Done:
- Paste a Spotify track / album / playlist / artist link (incl. `intl-xx` and `spotify:` URIs):
  preview with cover, song count, length; "Download N songs" queues one row per song, in order
- Metadata like Sunnify: public embed pages, no login, no API key. Limits shown to the user:
  playlists show the first 100 songs, artists their top 10, playlist tracks have no album name
  (filled from YouTube Music). Optional **own** Client ID/secret in Settings → Spotify links
  uses the Web API (full playlists, ISRC, track/disc numbers) and falls back to embed pages
- Matching like spotDL: YouTube Music "Songs" search through its web API (~0.6 s per song),
  ISRC query first when known, scored on title + artist + duration (±3 s full marks, >30 s
  rejected) with penalties for live/remix/cover/re-recorded/… versions the Spotify title
  doesn't mention. Matches below 0.8 are downloaded but flagged "Check match: …" in amber;
  below 0.6 the song fails instead of downloading the wrong thing
- Tags: title, artists, album artist, album, track n/total, disc, date, ISRC, 640px Spotify
  cover; YouTube description/URL noise removed. Lyrics from LRCLIB: embedded (USLT) + synced
  `.lrc` next to the file (toggle in Settings)
- Album tracks named `01 Artist - Title`, playlists `Artist - Title`, one folder per
  album/playlist, `<name>.m3u8` in order
- Resume like Sunnify: re-pasting a link skips songs already on disk ("Already downloaded")
- Sync like spotDL, but safe: `.neria-sync.json` manifest per folder; songs no longer on the
  Spotify list show a "Review N removed" dialog, all unticked by default, ticked ones go to
  the Recycle Bin (only for complete lists, never for truncated ones)

- "Pick another match" (swap button on Spotify rows, amber when flagged or failed): lists the
  YouTube Music candidates with album, length difference and match %, marks the current one,
  free-text search, open-in-browser to listen, and "Use this" re-downloads in place
  (`--force-overwrites`) keeping Spotify's tags, cover and lyrics. Songs with no confident match
  now fail with "Pick one yourself" instead of silently skipping

- Audio check (`fingerprint.rs`, on by default, Settings → Spotify links): after each
  download, Spotify's public 30 s preview (`audioPreview.url` in the embed data) and the file
  are fingerprinted with ffmpeg's Chromaprint muxer; the preview is slid along the song for
  the best bit agreement. Measured: same master 0.98, same recording edited 0.95, same
  recording other master/length 0.76, re-recording 0.58, different song 0.57. Verdicts:
  ≥ 0.72 "✓ sounds like Spotify", 0.66–0.72 "close, maybe another master", below that a
  different recording → automatically tries up to two other good candidates (skipping other
  copies of a version that already failed), then goes back to the closest and flags it
  "Doesn't sound like the Spotify preview (58% similar). Pick another?". ~1 s per song

- Reference preview fallbacks: Spotify's preview first, then Deezer's public API (exact ISRC,
  else search), then Apple's iTunes Search API. A fallback clip is only trusted if its title,
  artist and length (±3 s) match Spotify's, so a re-recording can't become the reference.
  Rows say which preview they matched ("✓ matches the Deezer preview"); with none anywhere
  they say "no preview to check against". Measured with Deezer as reference: original 0.88,
  re-recording 0.64 (cut-offs now 0.72 same / 0.66 unsure)
- Picker sound checks: when the dialog opens it finds the reference and checks the top 3
  matches plus same-title versions ruled out by length (often the album version of the same
  recording), in parallel; every other row has a "Check" button. YouTube refuses plain ffmpeg
  reads, so yt-dlp fetches the candidate's audio to the cache (~10 s each, deleted after).
  "✓ Same recording" rows float to the top. On Bonnie Tyler this surfaces the 6:58 album
  original (76%) above the re-recordings

Still to do:
- Faster candidate checks (yt-dlp start-up + YouTube's JS challenge dominate the ~10 s)
- No-preview-anywhere path is written but not exercised on a real track
- Own-key Web API path is written but untested (needs a real Client ID/secret)
- Spotify playlists > 100 songs without a key: no workaround yet beyond the notice
- Output note: source is YouTube Music (~128–160 kbps Opus/AAC); FLAC/WAV don't add quality
- Risks: Spotify keeps tightening its Web API (Feb 2026: Development Mode needs Premium,
  5-user cap, fewer endpoints), and YouTube Music's web API isn't official; both can break,
  see `docs/UPSTREAM.md` for where to look first

- Reference: Sunnify (sunnypatell/sunnify-spotify-downloader) does exactly this flow
  (public embed pages, no login; YouTube match on title + artist + duration). Its code is under a
  custom educational-use license, so Neria reimplements the approach rather than copying code.

Not planned: Zotify-style downloading of Spotify's own encrypted streams, or streamrip-style
downloading from Qobuz/Tidal/Deezer accounts (DRM/protection circumvention; gets accounts banned).
SoundCloud already works through yt-dlp.

## Phase 6: UI overhaul (before Android)
The features grew page by page; before porting, make the UI one coherent system that can
also scale down to a phone.
- Design system: shared tokens (colors, spacing, radii, type scale) and one component set
  (buttons, chips, rows, dialogs, toasts) instead of per-page variations; split `app.slint`
  into per-page/component files
- Layouts that adapt to window width (wide desktop → narrow), so the same screens can become
  the Android layouts
- Queue: group album/playlist songs under one collapsible card with overall progress;
  filters (active / done / needs attention); clearer flagged-match states
- Consistent dialogs (match picker, sync review, settings) and toasts for results like
  "Saved" / "Moved to Recycle Bin"
- Drag and drop files and links anywhere; keyboard navigation and focus states everywhere
- Settings reorganised into sections that match the pages (Download, Clip, Compress, Spotify)
- First-run onboarding, better empty states, light theme option, accessibility pass
  (contrast, screen-reader labels)

## Phase 7: Android
- Same `neria-core`; Slint Android UI (built on the Phase 6 adaptive layouts); embedded Python
  for yt-dlp; ffmpeg + libmpv as `.so`
- Share-sheet target ("Share → Neria"); APK via GitHub / F-Droid (Play Store bans downloaders)
