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
- Polish still to do: drag-and-drop files from Explorer; volume control

## Phase 3: Clip before download ✅
- "Clip first" on a link preview, or "Paste link" on the Clip page, streams the link in mpv
  (mpv's yt-dlp hook, 720p preview) with a filmstrip built from a low-res stream
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

- Implemented natively in `neria-core` (not by bundling spotdl.exe or Sunnify), so it uses our
  auto-updated yt-dlp, our queue/progress UI, and one row per track
- Best-of-both design (spotDL is MIT, so its algorithms can be ported with credit; Sunnify's
  license is custom, so only its ideas are reused):
  - Metadata like Sunnify: public embed pages, no login, no API key (spotDL's shared API key
    hits 24h rate limits). Optional **own** Client ID/secret in Advanced (never a shared key)
    for artist pages / big playlists; falls back to embed pages if the key is rate-limited
  - Matching like spotDL: search YouTube **Music** "songs" first (official audio, not music
    videos), then YouTube; score title + artist + duration; ISRC when available
  - Resume like Sunnify: per-folder manifest, skip tracks already on disk
  - Sync like spotDL: re-run a playlist to add new tracks. Tracks removed from the playlist are
    never deleted silently: a review screen lists them with checkboxes (default: keep), and
    "Delete selected" moves them to the Recycle Bin
  - Tags: full set from both (title, artists, album, album artist, track/disc no., year, genre,
    ISRC, cover art) plus lyrics: synced .lrc next to the file and embedded, plain-text fallback
  - M3U playlist file per playlist (from spotDL)
  - Upstream baselines and what to re-check later: `docs/UPSTREAM.md`
  - "Wrong match? pick another" button on each track (neither tool has this)
- Tags: title, artist, album, track number, year, cover art; optional synced lyrics (LRC)
- Output: MP3 / M4A / Opus / FLAC (note: source is YouTube Music, ~128–256 kbps; FLAC
  doesn't add quality)
- Risks: Spotify keeps tightening its Web API (Feb 2026: Development Mode needs Premium,
  5-user cap, fewer endpoints), so the no-login fallback matters

- Reference: Sunnify (sunnypatell/sunnify-spotify-downloader) does exactly this flow
  (public embed pages, no login; YouTube match on title + artist + duration). Its code is under a
  custom educational-use license, so Neria reimplements the approach rather than copying code.

Not planned: Zotify-style downloading of Spotify's own encrypted streams, or streamrip-style
downloading from Qobuz/Tidal/Deezer accounts (DRM/protection circumvention; gets accounts banned).
SoundCloud already works through yt-dlp.

## Phase 6: Android
- Same `neria-core`; Slint Android UI; embedded Python for yt-dlp; ffmpeg + libmpv as `.so`
- Share-sheet target ("Share → Neria"); APK via GitHub / F-Droid (Play Store bans downloaders)
