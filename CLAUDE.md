# Neria

Fast, portable, UX-first downloader / clipper / compressor. Rust + Slint, driving yt-dlp, ffmpeg,
Deno, gallery-dl (photos in posts) and libmpv, all auto-fetched into `bin/` next to the exe. Plan and status: `ROADMAP.md`.
UI design system: `docs/DESIGN.md` (Studio + Play); follow it for any new or changed screen,
including its "Adding a screen or a control" checklist. Screenshots of every page and dialog at
every width: `powershell -File scripts/ui-check.ps1` (sheets in `target/ui-check`;
`-Exe target/clip/debug/neria.exe` for a build in another target dir while `target/debug/neria.exe` runs).

## Layout
- `crates/neria-core`: everything non-UI (tool fetch, yt-dlp probe/download queue, clip export,
  compression, media info, Spotify: `spotify.rs` links/metadata, `ytmusic.rs` matching,
  `music.rs` tags/M3U/sync/Lyrics only/numbering, `lyrics.rs` lyrics sources (LRCLIB,
  Spotify's own with a sign-in), `post.rs` items of posts/playlists for the picker,
  `folders.rs` Audio/Video/Photos folders, `sound.rs` clip sound: gain/even out/mute/fades, `gif.rs` X GIFs + Make a GIF: GIF/WebP/MP4 and the size estimate, `picture.rs` turn/mirror/crop filters, `live.rs` live streams: rewind marks pinned to the clock, the replay buffer, the watch screen's tape (`Tape`: the stream to now, what the player can go back to, what a recording has), `ttv.rs` Twitch without its ads: a loopback relay serving the players that come without the ad (TTV-AB's idea), `chat.rs` live chat, read-only and never signed in (Twitch's chat server anonymously, YouTube's own chat feed; past streams: YouTube's replay through yt-dlp, Twitch's saved chat paged by second; 7TV/BTTV/FFZ emotes, emoji as Noto pictures; `.chat.json` next to a video, `t` seconds into it), `chat_draw.rs` drawing chat (one renderer for the panel's rows and burned-in frames: Inter Tight + Windows' fonts behind it, tiny-skia), `chat_out.rs` chat into a clip (a track: ASS in .mkv, mov_text in .mp4; burned in beside or over the picture, a frame only when chat changes), `record.rs` recording a live stream (its chat saved next to it, from the start / from now, parts joined on Stop or at the next start, holes and getting them back (`repair_holes`, spliced by sound), Clip the last…; `fmt::live_row` words its queue row), `timeline.rs` the clip timeline's zoom: view maths, ruler steps, filmstrip tile grid, sound levels (peaks) drawn for any part, `compress.rs` goals (`Goal`), the plan and its plain hint (`Look`), sound files (`plan_sound`: M4A (AAC), MP3 or Opus; in a list AAC for "Plays everywhere", else Opus), black bars, kept tracks, trimming, the before/after samples, how long a size holds), `capture.rs` instant replay of the screen or a game window (prototype, picture only, no UI yet): ffmpeg's `ddagrab` / `gfxcapture` straight into the GPU's encoder, the route tried at start (`Route::pick`: NVENC, AMF, Quick Sync, NVENC through memory, x264), pieces that overwrite the oldest, saving includes the piece still being written; `capture_sound.rs` its sound from WASAPI, a track each: the PC (or everything but one app), one app's process tree (Discord), a mic; raw PCM to ffmpeg over local sockets, silence filled by the clock; "Everything" (the mix) is the first track; `hotkey.rs` a key that works inside games (`RegisterHotKey`, no hook; F8 by default) and the "saved" sound, `transcribe.rs` speech to text, optional: whisper.cpp's `whisper-cli` + the large-v3-turbo model fetched on demand into `bin/whisper/` (CPU pack, or the NVIDIA pack), never at first run; sound joined in order, `.txt` / `.srt` written from its JSON), `voice.rs` "clip that": a phrase the user types, heard on the mic: sherpa-onnx keyword spotting (static, `=2.0.2`), its 5 MB model fetched on demand into `bin/voice/`, the phrase cut into the model's word pieces from its `bpe.model` (`Pieces`, unigram: best-scored split), `Prefs` (Settings → Voice), `Sayings` (Test it counts three by loudness), `spellings` / `try_spellings` (sound-alike spellings for a made-up word, tried on the test's sound), `Tuning::sensitivity` (Normal / High / Highest), mic through `capture_sound::Reader`; `voice_match.rs` your voice samples: each Test it adds a named set (`voice/samples/set-<ms>/`, 16 kHz WAVs + `name.txt`, up to 5) next to the exe, a saying counts if it matches any one set by that set's own strictness, MFCC fingerprints compared by dynamic time warping (a match must be about as close as the samples are to each other, and about as long), so an accent the phrase model misses still counts. Shared with the future Android app, so no UI code here.
- `crates/neria-player`: libmpv loaded at runtime (libloading) + OpenGL render API.
- `apps/desktop`: Slint UI in `ui/`:
  - `app.slint`: `AppWindow` only, the one API Rust sees (all properties/callbacks); wires them
    into the pages and overlays. Re-exports the structs and `Fmt` from `types.slint`, and the
    feature globals from `globals.slint` (`Accounts`, `Extras`, `Background`, `QueueActions`,
    `Presets`, `Names`, `Music` (lyrics sources, Lyrics only, numbering), `ClipSound` (Clip page → Sound), `ClipGif` (Make a GIF), `ClipPicture` (Picture mode), `ClipList` (the clip list), `ClipLive` (a live stream in the clipper), `ClipView` (the timeline's zoom and scroll; Rust owns the view), `ClipChat` (live chat: the panel, the activity strip, Chat in this clip, Settings' Save live chat), `Compress` (the whole Compress page: files, goals, before/after, Advanced, trim), `Voice` (Settings → Voice and the live player's Listening mark, flash and "Heard" toast)): newer features keep their state there and pages import them directly,
    instead of threading every property through `app.slint` (Rust: `ui.global::<Extras>()`).
  - `pages/`: `download`, `clip`, `compress`, `settings` (drawer), `setup` (first run).
  - `dialogs/`: `match` (pick another match), `sync` (removed songs), `spotify` (heads-up),
    `drop` (drag-and-drop overlay + clip/compress choice), `tracks` (captions / audio tracks /
    chapters picker), `background` (close-to-tray question, Download later), `names`
    (filename builder), `sound` (Clip page → Sound: volume, fades, sound only), `gif` (Make a GIF), `clips` (the clip list), `chat` (Chat in this clip: left out / a track / burned in, which messages), `transcribe` (speech to text: get it, choices, at work, saved; one dialog for the Clip page, dropped files and Extras → Transcript).
  - `components/`: `job-row`, `quality-row` (presets + Advanced), `pick-list`, `timeline` (+
    `ChapterStrip`), `extras-row`, `sidebar` (rail + compact `TabBar`), `update-banner`,
    `trim-strip` (Compress page's trim), `chat-panel` (the chat beside the player: a full-height column from 1200 px, beside the player on medium windows, a sheet on compact), `live-timeline` (watching a live stream: its tape in the timeline's place; a click goes back where the player can).
  - `widgets.slint`: the one shared control set (buttons, chips, `ChoiceChips`, `Tick`, `Tag`,
    `Notice`, `Divider`, `ListRow`, `Thumb`, `Sticker`/`LogoMark`, `EmptyState`, `Dialog`, `Menu`,
    `Toast`, `CropFrame`, `ChoiceRow` (a choice to compare: hint + value), `CompareSlider`
    (before/after)…). `theme.slint`'s `Screen` global holds the width breakpoints (wide / medium /
    compact, see `docs/DESIGN.md`); pages read `Screen.compact` etc. instead of taking the width.
    Reuse these instead of restyling per page. `theme.slint`: tokens + bundled fonts in `ui/fonts/`.
  - Pages take plain properties (no `clip-`/`cmp-` prefixes) and are bound in `app.slint`.

  Rust: `main.rs` (download page), `clipper.rs` (clip page; mpv renders into a GL texture shown
    as a Slint image), `compressor.rs` (compress page; kept parts, Volume), `cmp_player.rs` (its 10 s sample played in the page, Now / After: its own mpv drawn from the rendering hook in `clipper.rs`), `signin.rs` (`neria --sign-in <site>`:
    the WebView2 sign-in window, its own process) + `signin_ui.rs`, `extras_ui.rs` (captions,
    audio tracks, chapters), `background_ui.rs` (tray, notifications, Download later),
    `names_ui.rs` (filename builder), `sound_ui.rs` (Clip page → Sound), `gif_ui.rs` (Make a GIF), `chat_ui.rs` (live chat on the Clip page: live / a past stream / a file's `.chat.json`, rows drawn to fit the panel, the activity strip, a clip's chat), `transcribe_ui.rs` (the Transcribe dialog, Settings → Speech to text, Extras → Transcript), `voice_ui.rs` ("clip that": one mic thread, listening only while the live player is open or Settings → Voice shows; Test it; hearing it = Clip the last while recording (Undo deletes it), Save the last while watching), `picture_ui.rs` (Picture mode), `clips_ui.rs` (clip list), `timeline_ui.rs` (the timeline's zoom: view, waveform from peaks, close-up frames), `order_ui.rs` (queue order: the list is the truth, the
    queue is told the order it shows), `bulk_ui.rs` (several links, Paste & download, clipboard
    watcher), `follow_ui.rs` (Follow, Settings → Following, the checks), `live_ui.rs` (a live link's preview line, recording rows' Stop / Clip the last, saving recordings on quit). Watch mode lives in
    `clipper.rs` (`Watch` global). Core: `cookies.rs` (per-site cookie files, only ever
    passed for links of their own site; each yt-dlp run gets its own copy), `tracks.rs`,
    `names.rs`, `follow.rs` (`following.json`).

## Commands
```bash
cargo test -p neria-core
cargo test -p neria --test ui_rules   # UI guardrails (colours, keyboard, labels, widths, contrast)
cargo run -p neria
cargo build --release -p neria
```
Rust isn't on PATH in fresh shells here: prefix with `$env:Path += ";$env:USERPROFILE\.cargo\bin"`.

Headless checks (tools must exist in `target/debug/bin`, i.e. run the app once):
- `NERIA_HOME=target/debug cargo run -p neria-core --example smoke -- <url> [audio|video] [out] [start end [exact]]`
  (posts/playlists: lists the items and downloads them all, or `NERIA_PICK=1,3`)
- `NERIA_HOME=target/debug cargo run -p neria-core --example clip -- <file> <start> <end> [fast|exact]`
  (sound: `--gain <dB>`, `--even <LUFS>`, `--mute`, `--fade-in <s>`, `--fade-out <s>`,
  `--audio-only <fmt>`; `--in-place <file>` changes a whole file as a link clip gets it;
  `--gif gif|webp|mp4 [--width] [--fps] [--once]` prints the size estimate, then makes it;
  picture: `--turn <1-3>`, `--flip-h`, `--flip-v`, `--aspect 9:16`, `--crop x,y,w,h`;
  `--clips a-b,c-d [--names A,B] [--join]` for the clip list)
- `NERIA_HOME=target/debug cargo run -p neria-core --example record -- <live link> <now|start|both> <seconds> [out]`
  (records then stops and saves; `both`: from now with the part before fetched next to it, as the app's From the start, `--started <unix>` with it; `--clip <s>@<t>`, `--kill-at <t>` a dropped connection,
  `--crash-at <t>` then `--save <work dir>`, `--queue` through the download queue)
- `NERIA_HOME=target/debug cargo run -p neria-core --example live -- <live link> window|rewind <s ago> <len>|buffer <min> <run s> <clip s> [out]`
  (how far back the site keeps it; a rewound part through the queue, `NERIA_WAIT` / `NERIA_NOW`
  to download the same mark later; the replay buffer)
- `NERIA_HOME=target/debug cargo run -p neria-core --example compress -- <file> <discord|email|whatsapp|sharp|tiny|mb|q0.0-1.0> [h264|hevc|av1] [cpu|gpu]`
  (`--range <a> <b>`, `--parts 10-20,60-75 [--each]` (joined, or a file each), `--even <LUFS>`, `--sound m4a|mp3|opus` (a sound file's format),
  `--first-only` (else every sound track, captions, chapters), `--bars`,
  `--preview` (size estimate + before/after frames, no encode), `--sample <at> <secs>`)
- `NERIA_HOME=target/debug cargo run -p neria-core --example capture -- [--window game.exe | --monitor 0] [--keep 60] [--run <s>] [--out dir]`
  (instant replay; prints fps / dropped frames every 2 s; Enter saves, `q` stops; `--routes`
  says which capture→encoder routes work here, `--route qsv|nvenc|amf|nvenc-copy|cpu` forces one;
  sound by default: "Game & PC (no Discord)", "Discord" (when it runs), "Mic" (Windows' default,
  or `--mic <part of its name>`; `--mics` lists them); `--no-sound`; `--sound pc,mic,app:X.exe,except:X.exe`;
  F8 saves from anywhere (`--hotkey Alt+F10`, `--no-hotkey`))
- `NERIA_HOME=target/debug cargo run -p neria-core --example chat -- <link> live <s> | replay [out.chat.json] | draw <chat.json> <out.png> [--at s] [--overlay] [--light] | into <video> <chat.json> <out> [--from a --to b] [--track|--overlay] [--highlights] [--no-bots]`
  (`-` for the link with draw / into; `record` saves a recording's chat by itself)
- `NERIA_HOME=target/debug cargo run -p neria-core --example voice -- [--phrase "clip that"]... [--mic <part of its name>] [--file <sound>] [--threshold 0.25] [--boost 1.5] [--run <s>]`
  (prints each phrase heard and the share of a CPU core; `--get` / `--remove` the model, `--pieces "<words>"`, `--mics`; `--samples a.wav,b.wav,c.wav --file <sound>`: every saying in the file compared with those voice samples)
- `NERIA_HOME=target/debug cargo run -p neria-core --example transcribe -- <file>... [--lang en] [--srt|--both] [--no-times] [--range a b] [--out dir]`
  (several files: one transcript, joined in order; `--status`, `--get [cpu|gpu]`, `--remove`)
- `NERIA_HOME=target/debug cargo run -p neria-core --example spotify -- <spotify link> <out_dir> [max_tracks] [--match-only] [--rematch]`
  (`--lyrics-only`, `--number`, `--add-lyrics`, `--no-lyrics`, `--no-verify`,
  `--no-spotify-preview`, `--spotify-lyrics`: see the example's header)
- `NERIA_HOME=target/debug cargo run -p neria-core --example lyrics -- <spotify link>... [--only lrclib|spotify] [--synced-only] [--show]`
  (what each lyrics source finds; `--token-check`: Spotify's web-player secret + an anonymous
  token, no account needed)
- `NERIA_HOME=target/debug cargo run -p neria-core --example verify -- <spotify track id> [--no-spotify] <file|youtube url>...`
  (audio check scores; `--no-spotify` forces the Deezer/Apple fallback reference)
- `NERIA_HOME=target/debug cargo run -p neria-core --example follow -- <channel or playlist link> [--try-new]`
  (what following it would see; `--try-new` really follows it, runs a check and downloads the
  newest upload: use a scratch NERIA_HOME with a `bin` junction)
  `smoke` also takes the Download page's picks: `NERIA_PRESET`, `NERIA_CONTAINER`, `NERIA_TEMPLATE`,
  `NERIA_CAPTIONS` / `NERIA_CAPTIONS_AUTO` / `NERIA_SUB_FORMAT` / `NERIA_SUB_MODE` / `NERIA_SUBS_ONLY`,
  `NERIA_AUDIO_LANGS`, `NERIA_CHAPTERS`, `NERIA_SOUND` (a link clip's Sound), `NERIA_GIF` (Make a GIF from a link clip), `NERIA_TRANSCRIPT` (`text`/`srt`/`both`: a transcript after the download), `NERIA_ASPECT` / `NERIA_TURN` (Picture) (see the example's header)
- `NERIA_HOME=target/debug cargo run -p neria-core --example updates -- [--any-age] [--update <tool>]`
  (new Neria release / newer tool builds; `--update` re-downloads into NERIA_HOME/bin, use a scratch copy)

Dev env hooks for the app: `NERIA_OPEN` (file or URL into the clipper), `NERIA_CLIP_IN` /
`NERIA_CLIP_OUT`, `NERIA_CLIP_PLAY` (start playing from In), `NERIA_CLIP_VIEW` (`start,span` in seconds: open the timeline zoomed), `NERIA_CLIP_LOOP` (loop on), `NERIA_CHAT_DEMO` (`1`, `dialog` or `sheet`, with `NERIA_OPEN`: sample chat from both sites in the panel, Chat in this clip open, the compact sheet), `NERIA_CHAT_EXPORT` (`panel`, `overlay` or `track`, `,light`: once a file with its `.chat.json` is open, set Chat that way and press Export), `NERIA_CHAT_DEBUG` (print Twitch chat's raw lines), `NERIA_CHAT_OFF` (no live chat), `NERIA_TRANSCRIBE` (files, `;` between: the Transcribe dialog as if dropped; `NERIA_TRANSCRIBE_START` presses Transcribe), `NERIA_FAKE_NVIDIA` (offer the NVIDIA pack), `NERIA_VOICE_DEMO` (`on`, `getting`, `test`, `test-miss`: Settings → Voice with sample data, with `NERIA_SETTINGS=voice`; `listening`, `heard`, `missing`: the live player's mark and toast, with `NERIA_WATCH` on a live link; nothing listens), `NERIA_COMPRESS` (files or folders onto the Compress page, `;` between), `NERIA_COMPRESS_START`
(auto-start once read), `NERIA_COMPRESS_GOAL` (`discord`, `email`, `whatsapp`, `sharp`, `tiny` or MB),
`NERIA_COMPRESS_TRIM` (`in,out` seconds: Trim open on that part), `NERIA_COMPRESS_PARTS` (`10-20,30-40`: those parts kept; `NERIA_COMPRESS_EACH`: a file each), `NERIA_COMPRESS_EVEN` (`14` / `16`: Volume → Even out), `NERIA_COMPRESS_FORMAT` (a sound file's: `m4a`, `mp3`, `opus`), `NERIA_COMPRESS_PLAY` (press the sample player once read), `NERIA_COMPRESS_ADVANCED` (the fold open), `NERIA_URL` (paste a link on the Download page), `NERIA_URL_START` (press
Download once a preview is ready, unless it's greyed out), `NERIA_REPORT` (copy the report of the first
failed download), `NERIA_PAGE` (`clip` or `compress`: open that page, empty), `NERIA_FILTER` (queue filter: `active`, `done` or `attention`), `NERIA_LYRICS_SPOTIFY` (tick Spotify's own lyrics, to see its warning and sign-in; `signed-in`: as if signed in), `NERIA_LYRICS_ONLY` (with a Spotify `NERIA_URL`: press "Lyrics only" once it's looked up), `NERIA_ADD_LYRICS` (a folder: "Add lyrics to songs I already have" on it, no picker), `NERIA_SETTINGS` (open the settings drawer; `download`, `following`, `spotify`, `network`, `general` (was `appearance`, still accepted), `voice` or `about` picks the section), `NERIA_COPIED` (a link: show the clipboard watcher's "Link copied" strip), `NERIA_WATCH` (a file or link: open it in watch mode), `NERIA_OWN_SOUND` (a `.wav`: pick it as "Your own sound…" for both sound menus, no file picker; `demo` / `missing`: show both menus on it with sample names, file there / gone, nothing saved), `NERIA_UPDATE_URL` (check this "latest
release" JSON instead of GitHub; test self-update with a local server and a copy of the exe),
`NERIA_UPDATE_INSTALL` (install a found update; `restart` also restarts), `NERIA_DEMO` (sample queue
rows in every state, for design checks), `NERIA_SYNC_REVIEW` (open the removed-songs dialog), `NERIA_DIALOG` (open a dialog with sample
data: `match`, `sync`, `spotify`, `pick` (post item picker), `toast`, `update`, `update-ready`, `update-failed`,
`signin` (a problem that sign-in fixes), `extras` / `captions` / `audio` / `chapters` (a video with all three, the
picker open), `advanced` (presets with Advanced open), `close` (close-to-tray question), `later`, `names`, `sound` / `sound-gain` / `sound-only` / `sound-as` (Clip page → Sound, with `NERIA_OPEN`; `sound-as` wants a sound file: Save as opus), `gif` (Make a GIF, with `NERIA_OPEN`: a real estimate), `transcribe` / `transcribe-get` / `transcribe-files` / `transcribe-work` / `transcribe-done` / `transcribe-failed` / `transcript` (speech to text, sample data), `picture` / `picture-result` / `clips` / `clips-page` (Picture mode, its result, the clip list; with `NERIA_OPEN`)),
`NERIA_MATCH_PICK` (open "pick another match" on the first flagged song),
`NERIA_WINDOW_POS` (`x,y` physical px: open the window there without taking the keyboard; `ui-check.ps1` puts its windows on another screen this way unless `-MainScreen`), `NERIA_WINDOW` (window size in logical px, e.g. `780x580`; resizing from outside with
MoveWindow/SetWindowPos also works, but in PowerShell don't name the window handle `$h` next to
a `$H` height: variable names ignore case, so the handle becomes the height and the window
jumps to 32767px tall; Windows doesn't enforce the 400x600 minimum on scripted resizes), `NERIA_FAKE_FREE_MB` (pretend the
drive has this much free, to test the free-space checks), `NERIA_DROP_HOVER` / `NERIA_DROP`
(a file path or link: show the drag-and-drop overlay for it / act as if it was dropped),
`NERIA_DROP_CLIPBOARD` (drop whatever is on the clipboard, through the real OLE drop code), `NERIA_TTV_ORDER` (`site,embed,…`: the Twitch relay's players in order; `site` first meets the pre-roll), `NERIA_TTV_OFF` (no Twitch relay), `NERIA_LIVE_FROM` (`now` / `start`: where a live link's recording starts, no click), `NERIA_REC_STOP` (seconds: press Stop on every live recording then), `NERIA_REC_EARLIER` (seconds: "Also get the … before" on the recording being watched then), `NERIA_REC_CLIP` (`10@40`: "Clip the last 10 s" on the first recording at 40 s), `NERIA_REC_MARK` (`30,70`: Start clip / End clip on it then), `NERIA_REC_WATCH` (seconds: watch it in the player then), `NERIA_REC_DEBUG` (print a recording's raw output lines, addresses cut), `NERIA_DROP_LOG` (a file: log what each real drag offers and what Neria made of it), `NERIA_MPV_LOG` (verbose mpv log file), `NERIA_LIVE_DEBUG` (a file: mpv's timing properties when a live stream opens), `NERIA_DEBUG_ARGS` (print ffmpeg args for compression),
`NERIA_HOME` (override app dir; for UI tests point it at a scratch folder with its own
`settings.json` and a junction `bin` → `target/debug/bin`, so downloads don't land in the real
Videos folder).
The window can be captured even when covered with `PrintWindow(..., PW_RENDERFULLCONTENT)`.

## Rules
- Portable: settings and tools live next to the exe; nothing system-wide. One exception:
  notifications need Windows to know Neria's app id, a per-user registry entry
  (`HKCU\Software\Classes\AppUserModelId\…`, name and icon only) that `background_ui.rs`
  writes while notifications are on and removes when they're turned off. No others.
- Spotify support = spotDL/Sunnify approach only (metadata from Spotify, audio from YouTube Music).
  Never add Zotify/streamrip-style downloading of DRM-protected streams. One exception, asked
  for by the user: Spotify's own synced lyrics through an opt-in Spotify sign-in (off by
  default, with a "use a spare account" warning), for lyrics only, never audio, metadata or
  playlists (`lyrics.rs`, see `docs/UPSTREAM.md` → Lyrics sources). Nothing (keys, secrets,
  code) is fetched from third-party repos at runtime.
- spotDL and Sunnify are references, not dependencies. `docs/UPSTREAM.md` records the upstream
  commit we last studied. When Spotify/YouTube matching breaks, check upstream changes since that
  baseline first, port the fix (spotDL is MIT; Sunnify is ideas-only), then move the baseline and
  add a review-log entry.
