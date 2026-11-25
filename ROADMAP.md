# Neria roadmap

Fast, portable, UX-first downloader / clipper / compressor. Rust + Slint, driving yt-dlp,
FFmpeg, Deno, gallery-dl (photos in posts) and libmpv, all auto-fetched into `bin/`.

**UX is the priority.** Every feature ships with a clear UI: plain-language status, sensible
defaults, no dead ends, nothing that needs a manual to understand. A feature that works but
feels confusing isn't done.

**Order:** "Up next" below is the one list of what we do and in what order (reordered
2026-11-21). The sections after it are the details: what each track built and its open bits.
Within a section, items are in the order we'll do them; related ideas are merged into one item
so they get built together.

## Shipped ✅

### v0.1: the basics
- **Downloader**: first-run auto-fetch of yt-dlp, FFmpeg, Deno; daily `yt-dlp -U`. Paste link
  → instant preview (title, thumbnail, channel, length). Quality chips (Best / 1080p / 720p /
  480p / Audio only), queue with live progress, cancel/retry. Advanced drawer: container,
  H.264, audio format, embeds, SponsorBlock, playlists, cookies, rate limit, concurrency,
  filename template, raw yt-dlp args
- **Clipper (local files)**: `neria-player` crate (libmpv, auto-fetched, loaded at runtime,
  rendered into a GL texture shown by Slint). Timeline with filmstrip + audio waveform, drag
  in/out handles, scrubbing, frame stepping, shortcuts (Space/K, I/O, J/L and arrows, `,` `.`,
  Home/End). Export lossless (fast, keyframe-snapped) or re-encode (frame-exact H.264); audio
  files too. Lossless mode shows keyframe ticks, highlights in amber any extra footage before
  the in point, with a one-click "Snap to keyframe". Playback stops at Out; loop toggle (R).
  Player box takes the video's own shape. Scissors button on finished downloads
- **Clip before download**: "Clip first" on a link preview, or "Paste link" on the Clip page,
  streams the link in mpv (720p preview). Filmstrip from the site's storyboards. Download clip
  sends only the selected range (`--download-sections`); Exact adds
  `--force-keyframes-at-cuts`
- **Compressor**: fit a size (20 MB Discord / 25 / 50 / 100 / custom) or pick a quality.
  Original resolution by default; Auto resolution/fps as an option; manual caps; audio bitrate
  or remove; stereo downmix. H.264 / H.265 / AV1; GPU encoders (NVENC / Quick Sync / AMF)
  detected by a real test encode, CPU fallback with two-pass. Guaranteed fit (re-encodes
  smaller on overshoot, up to 3 tries); Compress button on downloads
- **Spotify links (spotDL-style)**: metadata from Spotify's public embed pages, audio from
  YouTube Music, tagged with Spotify's metadata. Track/album/playlist/artist links, scored
  matching with "Check match" flags, "Pick another match", audio check against
  Spotify/Deezer/Apple previews, LRCLIB lyrics, numbered folders + M3U, resume, safe sync with
  Recycle Bin review. Details: `docs/SPOTIFY.md`

### v0.2.0: friends test
- **Clear answers for links Neria can't download** (`probe::explain`,
  `spotify::unsupported_reason`). Errors that won't change (unsupported site, removed, private,
  DRM, wrong country) show in red with what does work, and Download is greyed out. Errors that
  might pass (offline, rate limit, sign-in, age, members-only, not live yet) show in amber with
  what to try; pasting the link again retries. Spotify podcasts/audiobooks/profiles/short links
  get their own message; links without `https://` work. Still open: region-locked Spotify
  tracks (embed page still lists them)
- **Report a problem** (`report.rs`): a bug button on failed rows copies versions, the settings
  that matter (never the Spotify key), the link, the error and the last 40 lines of yt-dlp
  output; the user folder shows as `%USERPROFILE%`. Settings → About has a general "Copy
  report". Not yet for Clip/Compress errors
- **Versions and updates** (`update.rs`, desktop `updates_ui.rs`): Settings → About shows every
  tool's version. At start-up Neria checks GitHub releases of `PizzaSpark/neria`; a banner
  offers "Update now", swaps the exe in place (bin/ and settings.json untouched) and "Restart
  now". FFmpeg, Deno and libmpv get "Update tools" once a newer build exists and ours is over
  30 days old. Works because the repo is public
- **Auto-retry when YouTube breaks** (`probe::needs_newer_ytdlp`): a download that fails the
  way an outdated yt-dlp fails says "The site changed. Updating the downloader, then trying
  again…", runs `yt-dlp -U` (one job at a time, at most every 30 min) and retries once
- **Robust first start** (`setup_ui.rs`, `tools::explain`): the setup screen lists each tool
  (what it's for, its own meter, Ready / Stopped); failures get one plain sentence (offline,
  GitHub rate limit, cut-off download, full drive, antivirus) with "Try again" and "Copy
  details". **Writable folder check**: Neria unzipped into Program Files says "Move Neria
  somewhere else."
- **Spotify heads-up dialog** on the first Download: song info from Spotify, audio from YouTube
  Music (~128–160 kbps), each song checked and swappable; "Don't show this again"
- **Cookies hint**: Chrome, Edge, Brave, Opera and Vivaldi lock their cookies, Firefox works
  best; sign-in errors point to Settings → Network → Cookies
- **Free space check** (`disk.rs`): 200 MB spare to start, then the rest must fit (including
  the merge, re-checked every 3 s for livestreams); stops with "Not enough space on D: (381 MB
  free, needs about 1.5 GB)" and removes the partial files. Compress checks too
- **Drag and drop files from Explorer** (`drop_ui.rs`): an overlay says what will happen;
  Clip/Compress open the file, Download asks "Clip it / Compress it", audio goes to Clip
- **X GIFs saved as real GIFs** (`gif.rs`): X serves "GIFs" as short silent MP4s; Neria spots
  them, shows "GIF" instead of quality chips and turns the MP4 into a looping .gif
  (palettegen, width capped at 720). Settings → Extras keeps the MP4 instead
- **App icon**: an "N" whose left stem is a download arrow, on the yellow sticker
  (`design/icon/make_icon.py`, embedded by `build.rs`)
- **Release checks**: tag must match `Cargo.toml`; CI and release run `cargo fmt --check` and
  `cargo clippy -D warnings`; release notes from `docs/releases/<tag>.md`
- **UI foundation, first pass**: Studio + Play (`docs/DESIGN.md`): tokens and bundled fonts
  (`ui/theme.slint`), shared controls (`ui/widgets.slint`), Download page rebuilt, Clip /
  Compress / Settings restyled, Appearance (Same as Windows / Dark / Light), setup screen,
  one shared `Dialog` and `Tick` (match picker, sync review, update banner, toast)

### v0.3.0: posts, playlists, a queue you can trust, UI foundation
- **Pick what to download from posts and playlists** (`post.rs`): X, Instagram and Facebook
  posts with several photos/videos/GIFs, and playlists, list every item under the preview
  (thumbnail, name, length) with ticks, "Select all" / "None" and "Download 3 items". Each
  picked item is its own queue row. Each video row has its own quality menu ("As above" /
  Best / 1080p / 720p / 480p / Audio only). "Number the files in order" tick
- **Photos in posts**: from gallery-dl (new bundled tool, from `gdl-org/builds`) for X and
  Facebook (Facebook `share/` links resolved through yt-dlp first), from yt-dlp itself for
  Instagram; saved directly. Photo and GIF rows only offer Show (nothing to clip/compress)
- **Every X GIF in a post** becomes a .gif (before: only the last file of the job)
- **Honest post errors**: "Neria couldn't find a video in this post" (no pointless yt-dlp
  update any more); photos-only Facebook posts say the video couldn't be reached; Instagram's
  anonymous limit gets its own message pointing to Cookies
- **Folders** (`folders.rs`): `Neria\Audio`, `Neria\Video`, `Neria\Photos`; several items
  picked together get a folder named after the post/playlist, inside the kind's folder when
  they're all one kind, else next to them. Spotify albums synced before keep their old folder.
  Settings: "Sort into Audio, Video and Photos folders"
- **Save the thumbnail as a .jpg** next to the video (Settings → Extras, off by default)
- **Clip page for links** shows the Download page's quality chips and format button (same
  settings); X GIF links say "Saved as a .gif"
- **The container setting always applies**: single files that need no merge (YouTube format
  18) are remuxed to mp4/mkv; webm picks webm streams where the site has them
- **Checked on real links**: a single-GIF X post (saved as one .gif), Audio only on a
  playlist (`Audio\<playlist>\01 …`), a 315-video playlist (picker, one card, numbered
  `001 …` files, private/closed-channel videos fail with a clear reason, closing mid-way and
  reopening carries on). Queue updates reach the UI in batches, so big playlists don't flood
  it (closing used to hang behind them), and `queue.json` is written at most twice a second
- **Playlist folders like Spotify ones**: `<playlist>.m3u8` in the folder, kept up to date as
  items finish (keeps songs saved by an earlier download, numbered files in number order);
  items already in the folder are skipped ("Already downloaded")
- **One group card in the queue** per playlist/post/album: stacked thumbnail, "3 of 12 saved ·
  1 failed · now: …", overall bar, Pause all / Resume all / Cancel / Retry failed / Show
  folder; open by default, the chevron (or a click on the card) folds it. "Clear finished"
  leaves playlists that are still going; the "3 going" badge counts only what's running
- **Pause / resume**: Pause on downloading rows (not Spotify songs, which are matched and
  checked in steps), "Pause all" / "Resume all" next to the Queue title; yt-dlp keeps its
  `.part` files and Resume carries on from there; paused rows keep a grey bar ("687 KB of
  3.0 MB so far") and free their slot. Cancel removes the half-done pieces
- **The queue survives restarts** (`queue.json`): waiting and interrupted downloads come back
  and carry on once the tools are checked, paused ones stay paused, a playlist comes back
  whole (its finished rows too)
- **Duplicate check**: `history.json` remembers finished downloads by video ID; pasting one
  again says "You downloaded this today as mp3 · Show file", or "It's already in the queue",
  and the button becomes "Download again" (overwrites a file of the same name)
- **"This video is part of a playlist · Get the whole playlist"** on `watch?v=…&list=…`
  links (with "Allow playlists" off they only fetch the one video)
- **Queue thumbnails for playlist items**: YouTube sent WebP named `.jpg`, which didn't load

### v0.4.0: Download and Music tracks (2026-11-14)
- **Download track**, items 1–12 under "Then: feature tracks → Download": signed-in
  downloads, captions, audio tracks, chapters, tray + notifications + Download later,
  picker order, presets, filename builder, bulk links, queue order, Watch it first, Follow
- **Music track**, items 1–4 under "Music": synced lyrics (LRCLIB, opt-in Spotify/Musixmatch),
  Lyrics only, Add lyrics to songs you have, Number playlist songs
- Narrow windows: Download gets its own row under the link box
- Notes: `docs/releases/v0.4.0.md`

### v0.5.0: Clip track (2026-11-15)
- **Clip track**, items 1–9 under "Clip / edit": sound on export and Save sound only, Make
  a GIF and `.gif` files, the clip list, crop and shape, turn and mirror, the result
  preview, waveform for links, rewinding live streams and the replay buffer, and the
  timeline redesign (zoom and scroll, live tape)
- Download track follow-up: "Your own sound…" in both sound menus
- Notes: `docs/releases/v0.5.0.md`

### v0.6.0: Compress track (2026-11-15)
- **Compress track**, items 1–5 under "Compress": goals in plain words with what each will
  look like (Discord 20 MB, email 18 MB, WhatsApp 16 MB, two "look" goals, custom), before /
  after and a 10 s sample, several files and folders, trim with chapters, and an Advanced fold
  in plain words (format, graphics card, picture, sound, every track kept, black bars cut)
- Fixes: sizes in MB as upload limits count them, graphics cards missing the size (redone on
  the processor), sideways phone videos squashed when scaled
- Notes: `docs/releases/v0.6.0.md`

### v0.9.1: Capture settings, the graphics card for clips, calmer moves (2026-11-26)
- **Settings → Capture** (OBS-style: the page's choices in Settings too, Encoder out of
  Advanced), **Settings → General → Video work on** (clips, Sound/Picture, GIFs as MP4 and
  burn-ins on the graphics card, made again on the processor when it refuses), Speech to text's
  models and engine listed with sizes, each model removable on its own
- Moves: the drawer slides, a pill to the open section, a plain fade between pages, every page on
  the same edges; File names chips that really drag; shared controls behave like shadcn / Radix
- Fixes: Settings cut off the end of long sections; Capture's wrapped Encoder chips overlapped Sound
- Notes: `docs/releases/v0.9.1.md`

### v0.9.0: Capture, Phone, Library (2026-11-24)
- **Capture** (instant replay, F8, a track per app, Never record, Quality, "clip that" by voice),
  **Phone** (mirror by QR or cable, Clip the last 30 s, Record, Present, Wi-Fi hand-over), the
  **Library** (everything saved, Gone / Find it…, Duplicates, pick several), the **command
  palette** (Ctrl+K), **sound files** (Compress to M4A / MP3 / Opus / FLAC / WAV, waveform trim,
  several parts, Even out, Cut the silence, Edit tags, Put in folders, Paste times…), **Speech to
  text**, **Live chat** (beside the player, into clips), saved **Workflows** and your own **Tools**
- Fixes on the way: Neria's own yt-dlp updater (onedir build, start-up 1.3 → 0.45 s), quitting
  never hangs; CI no longer builds and uploads the exe (Actions storage), clippy 1.99's lints
- Notes: `docs/releases/v0.9.0.md`

### v0.8.0: One live screen (2026-11-16)
- **Watch, record and clip in the player**, items 8–10 under "Livestreams": Watch / Record on the
  preview, the player's bar by state, Twitch keeps the last 10 min while watching; "Also get the
  … before" joined by sound; Twitch without its ads (`ttv.rs`, TTV-AB's idea); missed bits
  fetched again and spliced in
- Fixes on the way: Twitch recordings' wrong length after its ad; the Download page's format
  button opens Advanced, "Highest (2160p)", the greyed Download lines up; Compress "working…"
- Notes: `docs/releases/v0.8.0.md`

### v0.7.0: Livestreams (2026-11-15)
- **Recording live streams**, items 1–7 under "Livestreams": from the start or from now, said
  on the preview; "Record when it starts"; the tape on the row (recorded, before you joined,
  missed bits, live edge); Stop saves, the stream ending saves, a crash saves at the next
  start; Watch it live, Start clip / End clip and Clip the last while it records
- Fixes on the way: recordings' yt-dlp/ffmpeg no longer outlive Neria
- Notes: `docs/releases/v0.7.0.md`

## Up next (priority order)
1. ✅ **Shipped v0.9.0** (2026-11-24; see "Shipped" above). The December hold was only for
   Actions usage, lifted once CI stopped building and uploading the exe
2. **Real-link checks left**: an Instagram carousel all the way through with cookies; whether
   Brave's cookies are read once Brave is fully closed; a scheduled stream / premiere recorded
   from its first second (Livestreams 2)
3. ✅ **"Clip that" by voice** (built 2026-11-21, Capture 5): in the live player, with Test it,
   voice samples in named sets, "undo that", the mark's menu. Left: the friend's accent
   re-tested with his own set
4. ✅ **Capture track's UI** (built 2026-11-21, Capture 4; mocks `design/capture.html`,
   `design/capture-tracks.html`): the Capture page, Automatic, a track per app, Never record,
   the clipper's Tracks, voice. Left: a real game session with F8 and "clip that" (only
   checked headlessly and with sample screens)
5. ✅ **Phone track** (built 2026-11-22, mocks `design/phone.html`, `design/phone-present.html`):
   the Capture page's Phone tab: connecting by QR, the mirror in the page at the phone's full
   size with its own replay buffer and "Clip the last 30 s", Record, Screenshot, Sound only,
   where its sound plays, Present in a window of Neria's own (with the sound), cable and Wi-Fi
   handing over. Checked end to end on a POCO X7 Pro. Left: a Discord viewer hearing a shared
   Present window; turning the phone (landscape) on the real phone; the Extras built
   2026-11-23 (camera, APK, files, clipboard) checked on a phone
6. ✅ **Neria's own tool updater** (built 2026-11-22, `ytdlp.rs`): yt-dlp's onedir build
   (`yt-dlp_win.zip`) replaces the one-file exe. Each release goes in `bin/yt-dlp/<version>/`,
   checked against the release's SHA2-256SUMS and by starting it, then the newest runs; an
   older folder goes once nothing runs from it, so a download in progress keeps its yt-dlp.
   Checked daily and when a site breaks (as `-U` was), and Settings → About offers a newer
   one with the other tools. Installs with the one-file exe move over at their next start
   ("Updating yt-dlp… 19 / 23 MB" on the start screen); the one-file exe stays the fallback
   when the onedir build can't be had. Measured: start-up 1.3 → 0.45 s, a short YouTube
   download 10.9 → 7.3 s, a Spotify candidate check 10.1 → 8.6 s. Checked headless (move
   over, update from an older release while it runs, two updates at once) and in the window:
   moving over, and "Update tools" landing 30 s into a 233 MB download, which finished on
   the release it began with; the next start removed that release and ran the new one.
   The tools check no longer lets one tool's failed check (GitHub's API limit) hide the
   others' updates; yt-dlp's check doesn't use the API
7. **Small open bits** (worked on 2026-11-23):
   - ✅ Live chat for "Clip the last" / "Save the last" while live: the clip gets its own
     `.chat.json` (checked on a YouTube recording: a 10 s clip with the 2 messages said in it).
     The player's "Save the last" uses the chat it reads; not yet checked in the window.
   - Kick's chat: the reader is written (Kick's public Pusher chat, emotes, deletions) but
     Kick's channel API answers Neria with HTTP 403 (Cloudflare; curl gets through), so a Kick
     link offers no chat yet. Getting past it would mean dressing Neria up as a browser: the
     user's call
   - ✅ Both chats of a multi-stream: "Also live on YouTube · Add its chat" in the chat panel
     (links in the description or the channel's links, else the same name with a matching
     title). Checked on IShowSpeed and ohnepixel live on both, both ways, in the window too
   - ✅ Speech to text: captions burned into a copy ("(captions).mp4", Transcribe dialog on a
     video), and ✅ Translate to English with the Best quality model (see Speech to text)
   - Spotify playlists over 100 songs: through the user's own key only (user's call
     2026-11-23: the web player's token stays for lyrics). Waiting on a key to test the
     own-key path
   - ✅ Phone extras built (not yet checked on a phone): the camera (Show: Screen / Back /
     Front camera, Android 12+, the mic with it), an APK dropped on the Phone tab installs, other
     files go to its Download folder, Share the clipboard both ways while mirroring
   ✅ **A Quality choice for capture** (built 2026-11-23, mock `design/capture-quality.html`,
   instead of OBS-style CBR / rate-control knobs): Smaller files / Standard / Best as rows on
   both Capture tabs, each with what it records and MB a minute for what's picked; Advanced
   with size, frame rate, bit rate (Custom, Back to Standard) and This PC's Encoder moved in.
   Standard is what recorded before (16 Mbit/s at 1440p60; the phone 8 Mbit/s at 60 fps);
   Smaller files scales to 1080p on the graphics card (Windows' own capture, `gfxcapture`:
   `scale_d3d11` won't take `ddagrab`'s frames); Best is 120 fps on a 120 Hz+ screen; the
   phone steps back from Best to Standard when its picture stalls. Checked headless on the
   laptop's 1440p 165 Hz screen (1920 × 1080 / 2560 × 1440 / 1440p120). Left: the sizes
   compared on the same game moment at each choice, a real game, AMD (untested here; a route
   that can't scale records full size), the phone on Best
8. **Bigger ideas** (section "Later"), in this order: ✅ Library page (built 2026-11-23, every
   action tried in the real app the same day; left: the grid view, see Later), ✅ command palette (built 2026-11-24), ✅ Audio
   tools (built 2026-11-24 into Compress, the Library and the Clip page; no page of its own), ✅ saved workflows (built 2026-11-24; see Later), ✅ external tools (built 2026-11-24; see Later). Parked (user's call 2026-11-23): the browser
   extension (Chrome's store asks $5 once; Firefox / Edge are free) and a Tampermonkey
   userscript instead (free everywhere, but two installs and no cookies); the clipboard
   watcher covers handing links over for now
9. **Before the mobile build**, once everything above is done:
   - **Code review** of the Clip track and the code from before the Download track (only the
     Download track has had one)
   - **Friends test round two** (they'll test then)
10. **Android**, on hold until then
Parked: **code signing** (SignPath Foundation, free for open source; SmartScreen's warning is
fine for now, user's call 2026-11-21).
Waiting on someone who can test it: YouTube Music Premium quality (Music 5).

## UI foundation ✅ (v0.3.0)
The features grew page by page. Make the UI one coherent system, so the features below are
built once on it. Friends' feedback feeds in here.
1. ✅ Split the pages out of `app.slint` into per-page/component files; one component set
   (buttons, chips, rows, dialogs, toasts) instead of per-page variations (`ui/pages/`,
   `ui/dialogs/`, `ui/components/`, shared controls in `ui/widgets.slint`)
2. ✅ Settings reorganised into sections: Download, Spotify, Network, Appearance, About, listed
   on the left of the drawer. Clip and Compress have no settings of their own (you pick
   everything per file there), so no empty sections. The format button and "What's new"
   open the drawer at Download / About; `NERIA_SETTINGS=<section>` for checks
3. ✅ Layouts that adapt to window width, from three breakpoints (`Screen` in `theme.slint`):
   wide 1000+, medium 720–999, compact under 720 down to a new 400 × 600 minimum (was
   780 × 580). Compact is the Android starting point (mocks: `design/narrow-layouts.html`):
   bottom tabs, one action per queue row plus a ⋯ menu, Settings as a full sheet with the
   sections along the top, Clip and Compress stacked with their main button across the bottom,
   dialogs edge to edge. Chip rows wrap at every width (Compress at medium used to clip);
   scrolling lists keep a 24px lane so the scrollbar never sits on a button.
   `NERIA_PAGE=clip|compress` and `NERIA_DIALOG=pick` for checks
4. ✅ Queue filters: All / Active / Done / Needs a look (failed, or a song in doubt), calm
   tabs with counts in the queue header (their own line below medium width); a card shows
   when any of its rows matches; an empty filter says so and offers "Show all". Flagged songs
   now come in two strengths: "Unsure" (weak title match, or close but maybe another edit) and
   "Sounds different" (the audio check says another recording), with Play (opens the file)
   and "It's right" (clears the doubt, kept in `queue.json`) next to Pick match; on compact
   they're in the ⋯ menu. Mocks: `design/queue-filters.html`. `NERIA_FILTER` for checks
5. ✅ Drag and drop links anywhere: Neria registers its own Windows drop target (winit only
   takes files), so a link or tab dragged from a browser opens in the clipper on Clip and goes
   into the Download box (with its preview) anywhere else; files work as before; the overlay
   says "Drop to download it". Keyboard: every control (buttons, chips, ticks, tabs, nav,
   menu rows, Settings sections) is reachable with Tab, pressed with Enter or Space, and shows
   a 2px ring while focused (not on mouse clicks); Esc closes dialogs and Settings, which take
   the keyboard when they open. `NERIA_DROP` / `NERIA_DROP_HOVER` take links too, and
   `NERIA_DROP_CLIPBOARD` drops what's on the clipboard through the real drop code
6. ✅ First run: "How it works" in the empty queue (paste or drag a link, pick a quality,
   where files go, Spotify and Clip first) until the first download or "Hide tips"
   (`tips_done` in settings); the link box takes the keyboard so Ctrl+V works at once. Clip
   and Compress, before a file is open: left-aligned buttons and "Recent downloads" from
   `history.json` (frame, length, age; Compress lists videos only), or one line on what the
   page is for. Accessibility: every control has a screen-reader role and label (icon-only
   buttons say what they do), progress bars report their value; colours checked against WCAG
   (`muted`, light status tags and accent text now 4.5:1, `dim` 3:1 and kept off body text,
   empty tick boxes get a 3:1 outline). Mocks: `design/onboarding.html`.
   Guardrails so later screens keep all this: `cargo test -p neria --test ui_rules` and
   `scripts/ui-check.ps1`, with a checklist in `docs/DESIGN.md`

## Feature tracks (built; open bits are in "Up next")
Grouped by page. Tracks can be interleaved; within a track, top first.

### Download
1. ✅ **Private and signed-in downloads**: content you can see when signed in: private and
   unlisted-to-you videos, age-restricted videos, members-only, friends-only and group posts,
   Instagram without the anonymous limit, your own Liked/Watch later lists. Today this only
   works through Settings → Network → Cookies, which fails on Chrome/Edge. Better: a "Sign in
   to this site" button on sign-in errors that opens a small sign-in window inside Neria and
   keeps that site's cookies in Neria's folder (portable, per site, "Sign out" in Settings);
   a `cookies.txt` import for power users. Cookies stay on this PC and are only sent to the
   site they belong to. (The browser extension under Later can hand over cookies too)
   Built: problems that signing in can fix ("age-restricted", "members only", "wants you to
   sign in", Instagram's limit, private) show **Sign in to YouTube** (or "Sign in again") and
   "or import cookies.txt". The window is `neria --sign-in <site>`, its own process: Edge
   WebView2 through wry with a Neria bar on top (Done; says "Signed in" once the site's login
   cookies appear), profile in `webview/<site>/`. Only cookies of that site go to
   `cookies/<site>.txt`; `cookies::args_for` hands that file to yt-dlp/gallery-dl for links
   of the same site only (else the browser setting, as before). Settings → Network → Signed
   in: sites with Sign out, sign in to a typed site, import a `cookies.txt` (split per site).
   The link is looked up again after signing in. `NERIA_DIALOG=signin` for checks
2. ✅ **Captions / subtitles**: a "Captions" picker on the preview listing every language,
   split into **Made by the creator** and **Auto-generated**, the video's own language on
   top; tick one or several (search for long lists, "All" / "None"). SRT (default), VTT, ASS
   or plain text. Separate files (`Title.en.srt`), embedded, or both. **Captions only**
   (`--skip-download`). Remembers the last picked languages; "No captions on this video"
   Built: a quiet line of buttons under the quality chips ("Captions: English +2") opens
   the shared picker dialog (`dialogs/tracks.slint`, state in the `Extras` global, Rust in
   `extras_ui.rs`): search past 8 languages, groups, All/None, format and mode chips,
   "Captions only". Several languages embedded = switchable tracks in the player. Text is
   made from SRT afterwards (`tracks::srt_to_text`); captions-only rows point at the caption
   file. Last languages, format and mode are kept in settings. "No captions on this video"
   shows for YouTube videos without any. Also fixed: a preview with an empty queue no longer
   spreads the free space between the title and the chips. `NERIA_DIALOG=extras|captions`
3. ✅ **Audio tracks (languages)**: YouTube videos with several audio tracks (dubbed languages,
   descriptive audio, original): an "Audio" picker listing each track by language with
   "Original" marked (the default). Several ticked = separate tracks in one MKV/MP4
   (`--audio-multistreams`), named by language so players show a track menu; Audio only +
   several = one file per language (`Title (Spanish).mp3`). Shown only when there's a choice
   Built: "Audio: Original (English)" next to Captions when the formats carry two or more
   languages (`tracks::audio_tracks`, original from the format note / language preference).
   Picks become `-f bv*+ba[language=en]+ba[language=es]/…` with `--audio-multistreams`
   (yt-dlp's merger tags each track's language); Audio only + several = one row per
   language in a group, `Title [id] (Spanish).mp3`. `NERIA_DIALOG=audio`
4. ✅ **Chapters**: a "Chapters" picker on the preview and in the clipper (chapter marks on the
   timeline, click one to select it). Tick any chapters; only those parts are downloaded
   (`--download-sections` per chapter). **Separate files** (`03 Chapter title.mp4` in a
   folder named after the video) or **One file** (neighbours 2 + 3 become one range; gaps
   2 + 4 are joined with a clean cut and the UI says "skips chapter 3"). In the clipper,
   chapters set In/Out or fill the clip render queue. Audio only works the same (one song
   per chapter from a full album upload, numbered and tagged)
   Built: "Chapters: all 9" on the preview opens the picker (`tracks::chapters`), with
   One file / Separate files (kept in settings) and a line saying what you'll get. One
   file: all = the whole video; one range = a clean-cut section; several ranges =
   `--download-sections` per range (neighbours merged, keyframes forced at the cuts) as
   `.neria-partNNN` pieces joined with ffmpeg's concat (`Title (chapters).mp4`). Separate:
   one row per chapter in a group, `03 Chapter title` in a folder named after the video,
   title and track number tagged (`--postprocessor-args Metadata+ffmpeg_o:…`); captions
   are saved once in that folder. Clipper: ffprobe reads a file's chapters (links reuse the
   preview's); a chapter strip under the timeline sets In/Out to the clicked chapter.
   Left for the Clip track: filling the clip render queue (it doesn't exist yet).
   `NERIA_DIALOG=chapters`; ui-check makes a chaptered sample (`clip-ch`)
5. ✅ **Tray, notifications and scheduling**: closing to the tray keeps downloads running
   (optional). A Windows notification when a download/compress/export finishes or fails
   (thumbnail, title, "Open" / "Show in folder"; a group sends one "12 songs saved"). Settings:
   on/off, only when Neria isn't focused, an optional sound (Neria chime, Windows' default or
   none; separate "failed" sound). **Download later** on the Download button's menu: pick a
   time or "overnight", the row waits as "Starts at 02:00" (needs the tray)
   Built: closing with downloads going asks "Keep going in the tray" / "Stop and quit"
   (Don't ask again); Settings → General (was Appearance): When I close Neria (Ask / Tray /
   Quit), notifications on/off, only when not looking at Neria, sounds for done and failed
   (Neria chime, made on the fly as a WAV; Windows' sound; none). The window hides instead
   of closing (`run_event_loop_until_quit`); the tray icon (tray-icon) brings it back and has
   Show / Pause all or Resume all / Quit, and a toast says once that Neria went on there.
   Toasts (tauri-winrt-notification) with the thumbnail and Open / Show in folder; a group
   sends one ("12 songs saved", "11 of 12 saved, 1 failed"); clip and compress send theirs.
   Windows only shows toasts for a known app id, so Neria writes one per-user registry key
   (`HKCU\Software\Classes\AppUserModelId\Neria.Downloader`: name and icon, no settings;
   removed when notifications are turned off). Download is now a split button: ▾ →
   Download now / Download later… (In an hour, Tonight 02:00, Tomorrow 08:00, At a time) /
   Tonight at 02:00; rows wait as "Later · Starts at 02:00" with Start now, kept across
   restarts. `NERIA_DIALOG=close|later`, `NERIA_SETTINGS=general`
   Built: "Your own sound…" in both sound menus. It asks for a `.wav` (RIFF/WAVE, up to
   15 seconds by its header, 10 MB), plays it once ("Trying doorbell.wav…") to try it (a synchronous `PlaySound` on a thread, since the async one
   says yes even to files it can't play) and only then keeps it as `sounds/done.wav` /
   `failed.wav` next to the exe (the original name in settings, for display). Under the menu:
   the file name, ▷ Play and Change… (the old sound stays until the new one has played).
   Choosing it again reuses the copy. If the file goes missing, the chime plays instead, a
   toast says so once a session, and Settings shows a warning with Pick again…
   `NERIA_OWN_SOUND=<wav>|demo|missing`
6. ✅ **More control in the picker**: drag rows into your own order before downloading (numbers
   follow your order); "Reverse" for newest-first playlists; remember the order on
   re-download; a container/audio format per item (today Settings' format applies to all)
   Built: a ≡ handle on each row (`DragHandle` in widgets.slint: drag it, or Tab to it and
   ↑ / ↓; a line shows where it lands), the file numbers ("01") shown next to the ticks
   when numbering is on, "Reverse" next to Select all. The row's menu holds quality and
   format together (As above, Best, 1080p…, Best · mkv/webm, Audio · mp3/m4a/opus/flac/wav,
   `pick_choice`); `DownloadRequest.format` overrides Settings for that item. The order is
   kept per playlist link in `orders.json` (50 lists) and used the next time it's opened;
   the playlist file (M3U) follows the numbers
7. ✅ **Simple format presets** instead of codec/container talk: Best quality · Best
   compatibility (H.264 MP4, plays everywhere) · Smallest file · Audio only; the current
   chips/containers move under Advanced
   Built: the chips are now Best quality · Compatible · Smallest file · Audio only
   (`settings::Preset`, `Presets` global); "Advanced" folds out "Up to" Any/1080p/720p/480p
   and "As" mp4/mkv/webm (or the audio formats for Audio only), with a line saying what
   Compatible / Smallest mean. Compatible = MP4 + H.264/AAC; Smallest = MP4, ≤480p,
   `-S res:480,+size,+br`. A picker row's own format still wins. Old settings with
   "Audio only" as the quality move to the Audio preset. The clipper's link mode uses the
   same chips. `NERIA_DIALOG=advanced`
8. ✅ **Filename builder** (drag and drop, no yt-dlp template syntax): field chips (Title,
   Channel, Upload date, Video ID, Playlist, Playlist #, Resolution, Duration, Site…) dragged
   into a line with plain text between them; a `/` makes a folder. Live example using the
   current link; warns about characters Windows doesn't allow and names that would collide.
   Presets ("Title", "Channel - Title", "Date - Title", "Title [ID]", "Playlist / 01 Title"),
   save your own; "Advanced" still shows the raw `-o` template
   Built: Settings → Download → File names shows the name in words ("Title [Video ID]")
   and Change opens the builder (`dialogs/names.slint`, `Names` global, `names_ui.rs`,
   core `names.rs`): ready-made and saved names as chips (+ Save mine), the line of pieces
   (drag a piece left/right, or ←/→ and Delete on it; × removes), "Add text", field chips
   to click or drag up onto the line (they land at the end), the example (the previewed
   video's title when there is one), one warning at a time (characters Windows won't take,
   no field at all, names that could collide, a `/` with nothing on one side), and the raw
   template under Advanced (applied on Enter; templates with unknown parts stay raw). The
   template is still `filename_template`, so old settings keep working. Applies to single
   videos; picked playlist/post items keep "01 Title". Later, if wanted: dropping a field at
   an exact spot in the line. `NERIA_DIALOG=names`
9. ✅ **Bulk and quick download**: paste several links at once (one per line, or a text file) →
   one row per link, skipping duplicates. "Paste & download" with your defaults (skips the
   preview) and an optional clipboard watcher that offers detected links
   Built: two or more links in the box (one per line, spaces, commas, or glued together by
   the one-line box: `probe::split_links`), or a dropped `.txt`, list as "5 links" in the
   picker (`bulk_ui.rs`, `Bulk` global): each looked up, three at a time, with its title,
   length and picture; unticked with a tag when it's In the queue, Downloaded before, Twice
   in this list, a playlist/post or Spotify link (Paste it on its own) or Can't download.
   Download adds one plain row per ticked link, in the list's order (per-row quality menu
   as in the picker). Download ▾ → **Paste & download** (also Ctrl+Shift+V in the link box)
   queues every link on the clipboard with the chips as they are; Spotify links go to the
   box instead (they need their look-up). Settings → Download → "Offer links I copy in
   other apps" (off by default): Neria reads the clipboard only when Windows says
   something was copied, and only offers a lone link copied outside Neria: a "Link copied"
   strip on the Download page (Download / Preview / ✕), a notification from the tray.
   `NERIA_COPIED=<link>` for checks
10. ✅ **Queue order**: drag waiting rows to reorder; "Move to top" / "Move to bottom" (Ctrl+↑ /
    Ctrl+↓); running jobs stay put, the next free slot takes the top waiting row; groups move
    as one (and can be reordered inside)
    Built (`order_ui.rs`, core `Queue::reorder`): waiting rows now read top-down in the
    order they start, so a new link lands below the last waiting row instead of on top. A
    ≡ grip on waiting and paused rows (wide and medium; drag it, or ↑ / ↓ and Ctrl+↑ / ↓ on
    it) and ⋯ → Move to top / Move to bottom (compact: in the row's ⋯ menu, no grip).
    Running and finished rows stay put; a playlist card moves with its rows, which move
    among themselves. The list is the truth: after a change the queue is told the order it
    shows (and `queue.json` keeps it)
11. ✅ **Watch the link before downloading**: a play button on the preview that streams it (same
    player as "Clip first", without the trimming)
    Built: a yellow play mark on a single video's preview opens the Clip page as "Watch it
    first." (`Watch` global, `clipper.rs`): plays at once, no In/Out, frame steps or loop;
    Back, Clip this (turns In/Out on) and Download (the whole link with the chips, as from
    the preview). A file that was open in the clipper comes back on Back, with its In/Out.
    Listening volume and mute (M) next to the time, on the Clip page in both modes (the
    Clip track's "Preview volume"); playback pauses whenever you leave the Clip page.
    `NERIA_WATCH=<file or link>` for checks
12. ✅ Follow a channel/playlist and download new uploads on a schedule
    Built (core `follow.rs`, `following.json`; `follow_ui.rs`): "Follow" under the picker
    of a channel or playlist preview; Settings → Following lists them (how often: hourly /
    daily / weekly, checked when, new found, Check now, Pause / Resume, Unfollow). Only
    uploads new since you followed are downloaded, in the kind of file the chips said then,
    as one card ("Kurzgesagt · 2 new") into a folder named after the channel/playlist.
    Channels are read from their Videos tab, newest 40; playlists whole. An upload counts
    as seen once it downloaded (`history::ever_downloaded`, so a deleted file isn't fetched
    again) or failed three checks; premieres not yet live are left for later. Checks run
    one at a time while Neria runs (tray too), every minute looking for due ones; missed
    ones run 20 s after start. Channel links (`/@name`) now preview their videos instead
    of their tabs (`probe::channel_videos_url`). Example: `--example follow`.
    Spotify playlists too since 2026-11-24 (Music 6)

### Music (Spotify and lyrics)
Built 2026-11-14 (`lyrics.rs`, `music.rs`, Settings → Spotify → Lyrics). Ticked only where a
real-link check covered it; the rest is in `docs/TESTING-music-track.md`.
1. **Synced lyrics from more sources.** Order: LRCLIB → Spotify's own synced lyrics
   (Musixmatch, opt-in). Findings per source: `docs/UPSTREAM.md` →
   Lyrics sources
   - ✅ LRCLIB: synced for all of Random Access Memories ("Motherboard": instrumental), 49 of
     Today's Top Hits' 50 (the 50th: plain text only) and a "Spotify Singles" track. Its rate
     limit (429 after ~14 quick requests) is waited out, not read as "no lyrics"
   - ✅ Spotify's own synced lyrics ("Musixmatch via Spotify"), checked with a spare account
     signed in through Neria's window: synced for 11 of Random Access Memories' 13 (none on
     Spotify for "Get Lucky"; "Motherboard" is instrumental), ~0.2 s a song after one token;
     and the whole chain gave Today's Top Hits 50 of 50 synced ("Raindance", plain-only on
     LRCLIB, came from Spotify). The anonymous Musixmatch route now returns decoy lyrics, so
     this follows spotify-lyrics-api's idea (user's call, 2026-11-14): off by default, a "use
     a spare account" warning before the sign-in, lyrics only, secret read from Spotify's own
     script
   - Genius: dropped (user's call, 2026-11-14). It answers Neria's requests with 403 (bot
     protection), and its API gives no lyrics text
   - AZLyrics: dropped, the site now blocks automated requests ("request for access")
   - ✅ Each row says the source and "synced" / "plain text only" / "no lyrics found" (or
     "instrumental", or "no synced lyrics found" with Synced only); Settings has a tick per
     source and "Synced lyrics only" (checked: a plain-only song says "no synced lyrics
     found", all sources off says "no lyrics found")
2. ✅ **Lyrics only (Spotify links)**: a "Lyrics only" button next to the preview (and in the
   Download ▾ menu): `.lrc` when synced plus `.txt` with the words, named like the song would
   be (checked on 4 album songs headless, and on the whole album in the window). ✅ **Add lyrics to songs I already have** (Settings →
   Spotify → Lyrics): picks a folder, reads each song's tags (Spotify id from the folder's
   manifest), adds a `.lrc` and the words inside the song; skips songs that have lyrics
   already (checked on 3 playlist songs, headless and in the window)
   - Later, opt-in only: "Sign in to Spotify for more synced lyrics": now built as part of 1
3. ✅ Optional "Number playlist songs" (Settings → Spotify, off by default): `01 Artist -
   Title`, padded to the playlist's length. Turning it on or off, or the playlist being
   reordered, renames what's there (by Spotify id) instead of downloading again (checked on
   3 songs: renamed, "Already downloaded", M3U follows)
4. Loose ends from the Spotify work:
   - Faster candidate checks: measured, not changed. A check is ~8 s: yt-dlp start-up 1.3 s,
     YouTube page + JS challenge ~2.5 s, the audio ~4 s, and a smaller audio format doesn't
     help (1.5 MB took as long as 4 MB). The dialog already checks candidates in parallel.
     The real win was yt-dlp's onedir build (`yt-dlp_win.zip`, starts faster), which can't
     `-U` itself: ✅ now Neria's own tool updater fetches it (Up next 6), a check went
     10.1 → 8.6 s
   - ✅ No-preview-anywhere path exercised on a real track: a "Spotify Singles" recording with
     Spotify's preview set aside (`--no-spotify-preview`): Deezer and Apple rightly find
     nothing, the song downloads and says "no preview to check against"
   - Own-key Web API path is written but untested (needs a real Client ID/secret)
   - Spotify playlists > 100 songs without a key: no workaround yet beyond the notice
5. **YouTube Music Premium quality (untested, no Premium account to check with).** Spotify
   songs are fetched from `music.youtube.com` links, so a YouTube sign-in (Settings → Network,
   Download track 1) is already passed to yt-dlp for them, and `ba/b` already takes the best
   audio. With a Premium account that should be the 256 kbps AAC stream instead of ~130–160
   kbps, so in principle it works today. Still to do, once someone can test with Premium:
   - Check which stream yt-dlp actually picks for a few Spotify songs when signed in
   - Show it: "256 kbps (Premium)" on the song's row; a hint in Settings → Spotify
   - The default mp3 re-encodes it (lossy to lossy): suggest m4a, which keeps it as is
   - Same caution as Spotify lyrics: bulk downloads can get a YouTube account flagged, so
     suggest a spare Premium account
6. ✅ **Sync shown, and Follow for Spotify playlists** (2026-11-24, a friend's question: "my
   playlist got a new song, does Neria tell me?"). spotDL's sync is a command you run; this
   is the same idea made visible and automatic (`docs/SPOTIFY.md` → Resume and sync):
   - A playlist or album downloaded before says "N in your folder" in its preview and the
     button reads "Download N new" / "Up to date"; only the missing songs are queued
   - Follow works on Spotify playlists (Settings → Following, checked hourly / daily /
     weekly): added songs download into the playlist's folder; removed ones wait for "Review
     N removed" on the row (toast + notification once), never deleted by a check
   - Checked: `--example follow -- <Today's Top Hits> --try-new` (followed minus its last
     song: that song downloaded, a made-up removed song waited for review, a second check
     found nothing new and didn't list it twice); the preview and Settings → Following at
     1280 / 900 / 720 / 400 (`ui-check.ps1` states `dl-spotify-sync`, `set-follow`).
     Not clicked through in the window yet: Follow on a Spotify preview, Review from Settings
     → Following, "Up to date"

### Clip / edit
1. **Audio controls**, two separate things so it's clear what ends up in the file:
   - ✅ **Preview volume**: a volume slider + mute for listening (mpv `volume`); never changes
     the exported clip (built with Download track 11: `Watch.volume` / `Watch.muted`, M)
   - ✅ **Clip audio**, applied on export: keep / mute / gain (± dB) / **normalize** (ffmpeg
     `loudnorm`, presets like -14 LUFS for YouTube/TikTok/Discord, -16 for podcasts) / fade
     in / fade out, with a live preview. Fast · lossless still works: only the audio is
     re-encoded (say so)
   - ✅ "Save audio only": extract the selection as MP3/M4A/Opus
   Built (core `sound.rs`, `ClipSound` global, `dialogs/sound.slint`, `sound_ui.rs`): a
   "Sound: as is" button next to Fast / Exact opens the Sound dialog: Volume (As is ·
   Louder or quieter -12..+12 dB with a limiter above 0 · Even out to -14 or -16 LUFS ·
   Mute), Fade in / Fade out (0.5–3 s), and for files with a picture Save: The video / Sound
   only (mp3, m4a, opus, flac, wav). One filter chain serves the export and the player: the
   level plays live (mpv `af`), fades are added on export (the dialog says so). Evening out
   is two passes (measure, then `loudnorm` linear at 48 kHz). Fast · lossless with new
   sound copies the picture and re-encodes only the sound, both starting on the keyframe
   before In (fades counted from there); Mute drops the sound, so it stays lossless. Link
   clips get the sound changed after the download ("Changing the sound…" on the row).
   Silent videos and X GIFs have no Sound button (and "Sound only" on one says there's no
   sound). Below wide windows the Clip bar is two rows (status above the buttons) so nothing
   is squeezed; on compact windows the timeline is shorter, so a link's "Download clip" no
   longer falls off the bottom at 400 × 600. Checked on generated files: -14.5 LUFS with
   fades (target -14), -16.0 for a Sound only mp3, -14.0 on a 2 s clip, +6 dB exact, fades
   measured as ramps, sound and picture starting together in lossless webm and mp4 cuts from
   a non-keyframe In, mp3 files in Fast and Exact; a real YouTube link clip through the queue
   went from -24.5 to -13.9 LUFS ("Changing the sound…"); mpv's log shows the live loudnorm.
   Not yet: heard by a person, Cancel during "Changing the sound…" (should end Cancelled).
   `NERIA_DIALOG=sound|sound-gain|sound-only`; `--example clip` takes `--gain/--even/--mute/
   --fade-in/--fade-out/--audio-only/--in-place`; `--example smoke` takes `NERIA_SOUND`
2. **GIFs**:
   - ✅ **Open and trim a `.gif`** in the clipper (today only video and audio files open), save
     as GIF again or as MP4/WebP, which are much smaller
   - ✅ **Make a GIF** from any selection: size, fps and loop, with a file-size estimate
     (palettegen); also WebP/short MP4 "GIF-like" for Discord
   - Perfect loop (port of loopgif): dropped (user's call, 2026-11-14)
   Built (core `gif.rs`, `ClipGif` global, `dialogs/gif.slint`, `gif_ui.rs`): the Export
   button got a ▾ (Export clip / Make a GIF…). The dialog: Save as GIF · WebP · MP4 (each
   says what it's for), Size (320 / 480 / 720 / as it is, never made bigger), Smoothness
   (10 / 15 / 24 fps / as it is), Loop (forever / once; not for MP4), and "About 1.2 MB",
   worked out by really making 3 s from the middle of the selection and scaling (a newer
   choice stops the older estimate); over 10 MB (GIF) or 8 MB it says what would be smaller.
   GIFs get a palette made for the clip (palettegen diff + paletteuse bayer). A `.gif` opens
   in the clipper (Open file, drag and drop) with no Fast / Exact: Export clip saves it as a
   GIF again as it was, Make a GIF turns it into WebP or MP4. Link clips download the
   selection in best quality, then it's made into the GIF ("Turning it into a GIF…"; an MP4
   takes the downloaded file's name). Checked: GIF / WebP / MP4 from a generated video
   (estimate 1.1 MB, real 1.2 MB; WebP 509 KB vs 512 KB), trimming a GIF to GIF and to WebP
   (less than half the size), a YouTube link clip to GIF (3.8 MB) and MP4 (103 KB), the menu
   placement at 900 and 400 wide. Not yet: looked at by a person in an image viewer or
   Discord. `NERIA_DIALOG=gif`; `--example clip -- … --gif gif|webp|mp4 [--width] [--fps]
   [--once]`; `--example smoke` takes `NERIA_GIF`
3. ✅ **Clip render queue**: several In/Out ranges on one video, listed as clips (rename,
   reorder, remove, jump to each), exported in one go (files through a local export queue,
   links as several `--download-sections` jobs); option to join them into one file. Chapters
   (Download track) feed into this
   Built (`ClipList` global, `dialogs/clips.slint`, `clips_ui.rs`, core `clip::export_many`):
   a "Clips" button by the shortcuts line (compact: next to In/Out) opens the list: "Add
   0:12 – 0:20" (or A on the page), "Add all chapters" (named after them), each row with a
   ≡ grip (drag or ↑/↓), its name to type, its range, ▷ (In/Out to it) and ✕; "Join them
   into one file, in this order"; "Clear the list". Saved clips show as numbered bands on
   the timeline and Export becomes "Export 3 clips". Files: one file each (`video - Intro.mp4`,
   names made file-safe), or joined: the pieces are cut frame-exact (a lossless cut would
   carry extra footage at every seam), joined as they are, then the sound is changed once
   over the whole (fades at its ends only); Sound and Picture apply to every clip. Links:
   one download per clip as one queue card, or one download of all the ranges (overlaps
   merged) joined in time order. The list is cleared when another video opens.
4. ✅ **Crop and aspect ratio**: draw a crop box, or pick 9:16 (Shorts/TikTok), 1:1, 4:5, 16:9
   with a draggable frame
5. ✅ **Rotate and flip** (90° steps, mirror), applied on export (forces re-encode; say so)
6. ✅ **Preview the result** before exporting, with crop/rotate/audio changes applied (fades
   are still only heard in the saved file: the dialog says so)
   Built (core `picture.rs`, `ClipPicture` global, `CropFrame` in widgets.slint,
   `picture_ui.rs`): a "Picture" button next to Sound turns the page into Picture mode: the
   bottom bar becomes shape chips (Whole picture · Free · 9:16 · 1:1 · 4:5 · 16:9), Turn
   left / right, Mirror, Upside down, Reset and Done, and a crop frame sits on the video
   (drag it, drag a corner; keeps its shape; arrows / Shift+arrows). Order: turn, mirror,
   then crop, so the chips are the saved file's shape; the crop is fractions of the turned
   picture written as `iw`/`ih` expressions (even sides), so it fits links whose size is
   only known later. While editing the player shows the turned picture with the frame;
   after Done it plays the finished picture (mpv `vf`, updated while paused too) with a
   "Result: 9:16" tag. Any picture change re-encodes the video (Fast says frame-exact);
   Make a GIF applies it too; link clips get sound and picture in one pass after the
   download ("Cropping and turning the picture…"; a .webm becomes .mp4; only the sound
   changing keeps every stream, cover art too, and a new picture alone copies the sound).
   Phone footage (a rotation tag): ffprobe's rotation (`FileInfo.rotation`) is used, mpv's
   own turning is switched off while there are filters and the same turn goes first in
   them, so the player shows what ffmpeg saves; the player box is now upright for such
   videos with no changes too (it was letterboxed 16:9 before). Below 1200 px the Clip
   bar is two rows (the long Sound / Picture labels squeezed the status at ~1000).
   Checked: 9:16 / 4:5 / turn + mirror / 1:1 GIF sizes with ffprobe and a frame by eye,
   three named clips separate and joined (6.52 s for 6.5 s of clips, fades), a .webm
   cropped and turned in place to a 720×720 .mp4, an mp3 with cover art evened out in
   place (cover kept, -14.3 LUFS), a YouTube link clip cropped through the queue, a phone
   sample (90° tag): player result and saved frame match, the frame moved and resized with
   posted mouse drags, Export pressed in the app for three clips (three named files) and
   for a turned 1:1 crop (720×720), screenshots at every width (picture mode, result, clip
   list, timeline marks). Not yet: a person using it; link clip lists (one card of several
   downloads, or joined in time order) not run end to end; dragging rows in the clip list
   and typing a clip's name only built, not driven. `NERIA_DIALOG=picture|picture-result|clips|clips-page` (with
   `NERIA_OPEN`); `--example clip` takes `--turn/--flip-h/--flip-v/--aspect/--crop` and
   `--clips a-b,c-d [--names] [--join]`; `--example smoke` takes `NERIA_ASPECT`/`NERIA_TURN`
7. ✅ Links: waveform for streamed links (needs the audio); test live/very long streams
   Built (`media::waveform_link`): after the filmstrip, a link's smallest sound-only format
   is fetched into the cache (with the site's sign-in), drawn like a file's, and thrown away;
   cached per link, stopped when another source opens, only for links of a known length up
   to 3 h (never a live stream). A 90-minute video: 32 MB, ~45 s to fetch, 5 s to draw, so
   the waveform turns up after the filmstrip; seeking to 50:00 and marking In/Out worked
   (no part of it downloaded). A site with no sound-only format falls back to a small whole
   video, capped at 300 MB. Not yet: a non-YouTube link, cancelling mid-fetch. Live: see 8.
8. ✅ **Clip a livestream without recording all of it**, two ways:
   - ✅ **Rewind the live stream** (YouTube, Twitch and other sites that keep the last hours
     seekable): open the live link in the clipper, scrub back, mark In/Out, download only
     that part (`--download-sections` on the live stream; check how far back each site goes)
   - ✅ **Replay buffer** (any live stream, OBS-style): "Keep the last 10 minutes" (5 / 10 /
     30 / 60) records the stream with ffmpeg into short pieces and deletes the oldest as new
     ones arrive, so disk use stays fixed; "Clip the last…" joins what's kept and opens it in
     the clipper. A calm row in the queue shows it's buffering (size, how much is kept,
     Stop); keeps going in the tray. Use rewind where the site allows it, the buffer
     elsewhere
   Built (core `live.rs`, `ClipLive` global, `LiveRow` on the Clip page; `--example live`):
   a live link (mpv's yt-dlp info says `is_live`) gets a LIVE tag, times as "4:30 behind
   live" (marks as "In −5:00"), Go live, and no Fast / Exact, Clips or Make a GIF. How far
   back: YouTube keeps an hour (its HLS playlist is a DVR window, `playlist_duration/3600`;
   read on two streams; all three tried offer the rewindable formats); Twitch keeps nothing to rewind. **Rewind** (a window over a
   minute and a half): "Save the last 30 s / 1 min / 5 min / 10 min", or marks; the selection
   starts as the last half minute ("Save the last…" takes the clock, not mpv's length, which
   can stall while paused). A mark is pinned to the clock when the stream opens
   (`now − where mpv's cache ends`: mpv's `duration` lags it by three 5 s pieces) and turned
   back into seconds of the site's window when the download starts (`live::section_now`), so
   waiting in the queue doesn't shift it; a mark that has scrolled out fails with "the site
   keeps only the last 1:00:00. Mark it again" (checked: nothing downloaded), and one in the
   future (a wrong PC clock) says so instead of handing ffmpeg a bad range; a live part never
   retries or downloads without its section (it would record the stream). Parts are named by
   the clock: `Title (live 21.19.03, 0m30s).mp4`. yt-dlp can't cut the live-from-start (DASH)
   formats, so it's the HLS ones. Checked against the Starship stream's on-screen clock: the
   same mark downloaded right away and 3 minutes later starts on the same second, and a part
   marked in the app starts on the second the player showed at In (T+13:19:48 both; a 15 s
   part comes out ~19 s, cut on the 5 s pieces), and "Save the last 30 s" pressed after 5
   minutes paused starts on the same second as a direct fetch of that clock time. **Replay buffer** (sites that can't
   rewind, e.g. Twitch): "Keep the last 5 / 10 / 30 / 60 min" → "Start keeping": ffmpeg
   writes 6 s pieces into `replay/` next to Neria, wrapping so the space stays put (a 1 min
   buffer held ~49 MB at Twitch 1080p); "Save the last…" joins the newest pieces (not the
   one being written) into the Videos folder; the stream address is looked up again when
   ffmpeg stops; Stop keeping, another video, or quitting stops it and deletes the pieces
   (checked: quitting 2 s or 12 s after starting leaves no ffmpeg and no folder; leftovers
   from a crash go at the next start). Differences from the plan: the buffer's line is on
   the Clip page (not a queue row), saved parts go to the Videos folder with Show file
   (not straight into the clipper), and the close-to-tray question doesn't count it:
   closing with no downloads going quits and drops what was kept (with "Keep going in the
   tray" chosen it keeps going there). No hint yet of how much space 60 minutes takes
   (~2.5 GB at Twitch 1080p) and no free-space check. Not
   yet: a person using it; Sound / Picture changes on a live part (same post-step as any
   link clip, not run on a live one); other sites' windows. Dev hooks: `NERIA_LIVE_DEBUG`
   (mpv's timing when a live stream opens); ui-check `clip-live`, `clip-live-buffer`,
   `watch-live`
9. ✅ **Timeline redesign** (picked 2026-11-14; mocks: `design/clip-timeline.html`).
   Today the timeline always shows the whole video, so on a long video or an hour of live
   stream a short clip is a sliver hidden by two fat handles, with no zoom or scrolling.
   - **A · Zoom and scroll**, for every video: Ctrl+scroll / pinch zooms around the
     pointer, scroll or drag the strip to move along it, a thin overview strip on top shows
     the visible part (drag its window); a time ruler whose marks suit the zoom; thin
     handles with a grip and the clip as a see-through band you can drag; −, +, "Fit clip",
     "Whole video" and "12 s on screen". Thumbnails and waveform need more detail when
     zoomed in (today: 12 frames and one 1600 px waveform for the whole video)
   - **C · Live tape**, for live streams: marks read back from now ("−4:00", "now"), the
     live edge on the right, pulsing, and the view follows it while it shows the edge; the
     last 5 minutes by default, scroll left as far back as the site keeps; "Last 30 s"
     drops the clip at the edge to nudge
   Built (core `timeline.rs`, `ClipView` global, `components/timeline.slint`,
   `timeline_ui.rs`): Rust owns the view (start + span; clamping, zooming around a point,
   the live edge, paging along with the playhead are tested maths in core), the timeline
   only maps it to pixels. Top to bottom: an overview strip of the whole video with the part
   on screen framed (drag the frame, click elsewhere to jump; not on compact, where a thin
   bar along the top shows where you are), a ruler whose step suits the zoom (tenths under
   a second; live: "−4:00" … "now" counted from the edge), the filmstrip, the waveform.
   Ctrl+scroll zooms around the pointer (a Windows touchpad pinch arrives as that; a real
   pinch gesture isn't wired), scroll moves along, + / − zoom around the playhead, and the
   bar under the timeline has Zoom out / Zoom in / Fit clip / Whole video and "40 s on
   screen" (wide); compact has − + next to In / Out. Dragging the strip moves along when
   zoomed and still scrubs when the whole video shows; the ruler always scrubs; a click
   moves the playhead. Thin handles with a grip, the clip a see-through band with its
   length on a tag that is also the grip to move the clip (not the whole band: dragging
   inside the selection still scrubs, as before). While playing, the view pages along when
   the playhead runs off the right; a jump (Home, chapter, clip list ▷) out of sight
   centres it; scrolled away while it plays, it's left alone. The chapter strip, clip
   bands, keyframe ticks (thinned for what's on screen) and the early-start shading follow
   the zoom. Detail: the waveform is now drawn by Neria from sound levels measured once
   (200 per second, cached as `peaks-200.bin`; links measure the fetched sound, then throw
   it away as before) for exactly what's on screen, so it's sharp at any zoom (replaces
   ffmpeg's 1600 px picture); the filmstrip gets close-up frames on a fixed grid per zoom
   step (files: ffmpeg seeks, 6 at a time, nearest the middle first; links: cells of the
   site's storyboard, saved by the first look-up), made a moment after zooming stops and
   kept, so scrolling back is instant; until they arrive the whole video's frame shows.
   Live: opens on the last 5 minutes with the edge pulsing on the right ("LIVE"); the view
   follows the edge while it shows it, scrolling back stops following until it's in view
   again; zooming while following keeps the edge on the right. Live streams have no
   filmstrip or waveform (none before either). Difference from the plan: "Save the last
   30 s / 1 min / …" still saves at once (clock-based, checked against the stream's clock in
   item 8) instead of dropping a selection at the edge: mpv's length lags the edge by ~15 s,
   so a selection "at the edge" would end short of it; the selection still opens as the
   last 30 s. Also below 820 px the volume slider gives way to the time (mute stays), and
   compact's Go live is an icon (its text cut "0:24.93 behind live" short).
   Dev hooks: `NERIA_CLIP_VIEW=start,span`; `NERIA_WINDOW_POS=x,y` (and ui-check puts its
   windows on another screen, unfocused); ui-check `clip-zoom`, `clip-zoom-ch`,
   `clip-zoom-song`. Checked: screenshots at every width of a 10-minute file zoomed to 40 s
   (close-ups, overview frame, ruler, waveform, keyframe ticks), chapters zoomed, a
   sound-only file zoomed, and a real YouTube live stream (−5:00 … now, the edge, the last
   30 s at the edge); posted mouse messages: dragging the strip 300 px moved the view back
   ~10 s (ruler, waveform, overview frame and selection with it, playhead left alone), the
   wheel moved it forward (then doubled: a notch was only 5% of the view); dragging In moved
   it 3:32 → 3:30.06, dragging the length tag moved the whole clip (length kept), Fit clip
   and Whole video did what they say (the zoom row then got fixed so its buttons never shift:
   "Whole video" and the readout used to come and go); a silent sample playing with 3 s on
   screen paged along; a YouTube link: filmstrip and waveform as before (now from its sound
   levels), close-ups from its storyboard when zoomed; light theme; the other Clip states
   at 400 and 900. Not yet: a person using it; Ctrl+scroll (posted messages can't hold
   Ctrl, and faking the key would reach whatever has the keyboard); the live view following
   the edge over minutes

### Compress: understandable for everyone, advanced when needed
Built 2026-11-15 on branch `compress-track` (mock: `design/compress-track.html`, direction A
"goal list" picked; ideas and numbers from HandBrake's presets, see `docs/UPSTREAM.md`).
Ticked only where a real-file check covered it; the rest is in `docs/TESTING-compress-track.md`.
1. ✅ **Plain goals instead of numbers** (`compress::Goal`, `compressor.rs`): a list where every
   goal is a row with what it'll look like and its size for this file: **For Discord** (20 MB:
   Discord's free limit went back to 20 MB on 2026-08-14), **For email** (18 MB, not 25: mail
   grows attachments by a third, so 25 MB messages hold ~18 MB of file), **For WhatsApp**
   (16 MB, the lowest figure sources give), **Smaller, still sharp** (H.264 22, HandBrake's
   default), **As small as possible** (27 at 720p 30 fps, its "Social" ceiling), Custom size.
   Hints come from the plan for this file ("720p 30 fps, a bit softer", "blurry"), measured
   on the short side so phone videos say "720p" too. A goal that can't work says so in the row
   (warn colour): "already under 20 MB" (Compress turns into "Already fits"), "too long to
   look OK" with **"Keep the first 1:40"** (opens Trim at the longest part that fits), or, for
   the look goals on a file that's already packed tight, "won't get smaller". Checked: a 10 min
   file (Discord 360p, WhatsApp blurry with "Keep the first 8:13"), a 40 s film, a 20 s file
   that already fits everything, at 400 / 720 / 900 / 1000 / 1280 wide
2. ✅ **Plain-language options**, under an **Advanced** fold whose header line says what's set
   ("Plays everywhere · graphics card · picture as needed · sound 96 kbps"): Format (Plays
   everywhere / Smaller / Smallest; picking Smaller or Smallest with a sending goal warns it may
   not play there), "Use my graphics card (faster) · Intel Quick Sync" (or why it can't), Picture
   (Auto / Keep it / 1080p / 720p / 480p), Sound, and from HandBrake: **keep every sound track,
   the text captions and the chapters** (picture captions can't go in an .mp4: the technical
   line says how many were left out), **cut off the black bars** (found by looking at five
   places, on by default when found), deinterlacing interlaced files (only the combed frames,
   frame rate kept), a slower encoder setting when the size is tight. The technical line
   stays at the bottom of the fold. Fixes on the way: phone videos stored sideways were
   squashed when scaled (now turned first); an Intel graphics card overshot the size on every
   redo and failed (now the redo goes to the processor's two passes, which hit it: 12.3 MB →
   11.9 MB for a 12 MB goal); sizes now count MB the way the limits do (1,000,000 bytes), so
   19.6 MB no longer reads "18.7 MB" or "19.3 MB is a bit over 20 MB". Checked: a letterboxed
   film with 2 sound tracks, captions and chapters (1920×800 out, all kept), a sideways phone
   video (720×1280 out), an interlaced 576i file (25 fps kept)
3. ✅ **Before / after**: "312 MB → up to 20 MB · 94% smaller" under the verdict, and the same
   moment before and after in a slider (drag or ← →), from a 2 s sample encoded with the real
   settings a moment after the options settle; the look goals' sizes are measured from three
   2 s samples ("about 75.6 MB": the real encode came out 73.9 MB). "Play a 10 s sample" encodes
   10 s from the middle and opens it in your video player (HandBrake's live preview). After
   the run: the real size and "% smaller". Never blocks Compress. Not yet checked: Play a
   sample pressed in the window (the sample itself checked headless)
4. ✅ **Several files**: "Open videos…" picks several, "Open a folder…" takes the videos in it,
   dropping several files or a folder on the Compress page adds them; one goal for all, each row
   says what it'll get ("360p, a bit softer", "Already fits", "Can't fit"); files that already
   fit are skipped, one failing doesn't stop the rest, Cancel stops the list (finished files
   stay; "Compress 2 videos" goes on with the rest). Files are read three at a time. The button
   counts what will really be made ("Compress 3 videos", "Nothing to compress"). Checked: a
   folder of 5 with WhatsApp: 3 compressed under 16 MB (15.4 / 16.0 / 15.9), 2 skipped; Cancel
   pressed (a posted click) during the second: the first saved with Show, the second
   "Cancelled" with no half file left, the button offered the 2 left; a folder dropped on the
   page (`NERIA_DROP`) listed its 8 videos. Not yet checked: several files dragged from
   Explorer at once (the OLE drop takes them all now)
5. ✅ **Trim inside Compress**: "Trim…" on the file opens a filmstrip with two grips (drag, or
   ← → with Shift for 10 s), the times, "Whole video", and a chip per chapter; the hints,
   sizes and the start button ("Compress 0:18") follow the part; a trimmed part never counts
   as "already fits" (it isn't a file yet), nor does an .mkv or .avi (apps don't play them).
   Checked: 0:12–0:30 of the film (chapters moved with it: 0:00–0:08, 0:08–0:18); posted
   clicks: Trim opened the strip on the whole video, dragging the end grip set 0:28 and
   "Compress 0:28"; the before/after line follows a drag. Light theme checked (marks over
   video stay light). Known: with captions and chapters in
   the same .mp4, ffmpeg's QuickTime chapter track is broken (VLC/mpv read the chapters,
   Apple's players won't)
Dev hooks: `NERIA_COMPRESS` (files or folders, `;` between), `NERIA_COMPRESS_GOAL`,
`NERIA_COMPRESS_TRIM`, `NERIA_COMPRESS_ADVANCED`; ui-check `cmp-film`, `cmp-long`, `cmp-trim`,
`cmp-sharp`, `cmp-many`.

## Speech to text (optional)
Built 2026-11-18 (asked for after Adobe's and Gemini's transcribers failed on a Taglish
lecture; whisper large-v3 got it). Nothing is downloaded until someone uses it.
- [x] Engine: whisper.cpp's `whisper-cli` + `ggml-large-v3-turbo-q5_0` (575 MB) and Silero
  voice detection, into `bin/whisper/`; the NVIDIA pack (685 MB, cuBLAS) when there's a card,
  else the CPU pack (22 MB). Measured on an RTX 3060 laptop: a 12.7-min lecture in 24 s on the
  card, about 2.3× real time on the CPU. A short English prompt brings back punctuation
- [x] Transcript `.txt` (`[12:43]` before each line, or not) and/or `.srt` captions (cut to
  two lines); language detected (mixed talks too) or picked; several files joined in order
  ("Part 2 · …" lines, times running on), or one each
- [x] Where: Clip page Export ▾ → Transcribe… (the selection or all; a link is downloaded
  first), a dropped file or several (Clip / Compress / Transcribe), Extras → Transcript on
  the Download page (after the download, "Writing the transcript…" on its row),
  Settings → Download → Speech to text (status, Remove, switch to the NVIDIA pack)
- [x] Pick the model (built 2026-11-23): Settings → Download → Speech to text and the
  Transcribe… dialog choose **Fast** (`large-v3-turbo-q5_0`, 575 MB, the default) or **Best
  quality** (`ggml-large-v3-q5_0`, 1.08 GB: whisper.cpp's q5_0 of large-v3, as close to the
  3.1 GB full one as makes no difference). Best quality is downloaded the first time it's
  used, inside the job ("Get it and transcribe"), and Settings removes it on its own. On the
  RTX 3060 a 30 s clip: Fast 2.9 s, Best 4.2 s. The separate Python faster-whisper large-v3
  in `D:\APPS\Tools\Whisper` (5.1 GB) can be deleted once this ships
- [x] Translate to English ("Write it in English", picks Best quality: the turbo model can't
  translate). Checked on a Spanish TED talk: clean English captions, timed right
- [x] Captions burned into a copy of a video (see Up next 7)
- [ ] Later, if wanted: speaker names; a smaller model for slow PCs
Dev hooks: `NERIA_TRANSCRIBE` (+ `_START`), `NERIA_TRANSCRIPT` (smoke), `NERIA_FAKE_NVIDIA`;
ui-check `transcribe-get`, `transcribe`, `transcribe-files`, `transcribe-work`,
`transcribe-done`, `transcript`.

## Livestreams (own milestone)
Built 2026-11-15 on branch `live-track` (mock: `design/live-track.html`, picked 1A · 2A · 3A;
engine `record.rs`, row `fmt::live_row`, UI `live_ui.rs`; ideas from ytarchive and streamlink,
see `docs/UPSTREAM.md`). Ticked only where a real stream covered it; the rest is in
`docs/TESTING-live-track.md`.
1. ✅ **Two ways to record, picked by Neria and said on the preview** ("LIVE · Records from the
   start · about 22 min to catch up, then it follows live", with "Record from now instead";
   the button says "Record"). From the start when the site keeps it (YouTube with rewinding on,
   at most its last 120 h; Twitch's growing VOD playlist) and the stream began under 6 h ago;
   else from now ("it's been live for 11 days, longer than the site keeps" / "this stream
   can't be rewound"), with "Record from the start instead" while it's reachable. Measured:
   YouTube catches up ~6× real time, Twitch ~12×; both then follow live by themselves. A
   from-start the site refuses falls back to from now and says so. Every part is H.264 + AAC
   so parts of both kinds join without re-encoding (checked across the seam). Checked: Sky
   News and a lofi radio (YouTube, from now), YouTube from the start (headless, resumed after
   a drop), Twitch both ways (headless)
2. **Scheduled streams and premieres**: "SOON · Starts in 42 min", "Record when it starts";
   the row waits with a countdown (`--wait-for-video 30-60`) and records from the first
   second. Not yet checked on a real upcoming stream (none found on 2026-11-15)
3. ✅ **Stop that keeps the file**: Stop kills yt-dlp and Neria makes one file from the parts
   (a 75 s Sky News recording saved in under a second, 1080p, plays). The stream ending saves
   it too ("the stream ended", checked on a local test stream). Cancel throws it away, behind
   "Throw away what's recorded? Stop keeps it instead." A recording takes no download slot.
   Closing Neria asks ("A live stream is being recorded": keep recording in the tray / stop,
   save and quit; checked: saved, quit in ~1 s). A crash: its yt-dlp/ffmpeg end with Neria
   (a job object; they used to keep recording), and the next start saves the parts ("saved
   after Neria closed", checked by killing Neria mid-recording)
4. ✅ **The tape on the row**: stream start → now, hatched before you joined (a long stream
   shows a little before the join, not days of hatching), yellow recorded (striped while
   catching up), red ticks where something was missed, a pulsing red live edge; under it
   "you joined 1:03:12 ago · missed 0:42 at 31:05 · now". Holes come from ffmpeg's and
   yt-dlp's own "skipped" warnings and, between reconnects, from YouTube's piece numbers (an
   overlap is trimmed); where pieces aren't numbered, "missed a moment" (clock, no length).
   Checked: a dropped connection (killed yt-dlp) reconnected with the overlap trimmed and no
   false hole; a deleted piece on a local stream showed as missed. Not shown: "how far behind
   live" and segment counts (not measurable reliably), and the "⋯ → Details" panel
5. ✅ **Stats**: recorded length, size, resolution, speed on the row; "catching up: 51:10 of
   2:14:00 · live in ~17 min" while catching up; the saved row keeps length, size, why it
   ended and what was missed
6. ✅ **Clip the last…** (30 s · 1 min · 5 min · 10 min): "Clip the last ▾" next to Stop (a
   scissors icon at medium width, in ⋯ at 400 px), cut from what's recorded, no second
   download; saved as its own row with Clip/Compress. While catching up its menu says "You can
   clip once it has caught up" instead of the picks. Checked: 10 s from a Twitch recording
   through the queue (0.2 s), 10 s from a YouTube from-the-start recording (video and sound
   start together), and pressed in the app (`NERIA_REC_CLIP`) on a stream 73 min old that
   Neria recorded from the start by itself. Clipping a stream you're not recording: the clipper's live mode (Clip track 8)
7. ✅ **Watch it while it records, and clip as it happens** (asked for 2026-11-15): "Watch" on a
   recording's row (⋯ at 400 px) opens the stream live in the player ("Watching it live.";
   no Download there, it's being recorded). **Start clip** (the row's Clip ▾, or the player)
   begins 10 s back so a late press keeps the first words; **End clip** saves from there to now
   as its own row while the recording goes on ("clipping from 0:20 · 0:25 so far" under the
   tape; Cancel clip in ⋯). Cut from what's recorded, no second download. Checked in the app:
   Start at 30 s, End at 70 s gave a 50 s clip, from the row and from the player, at 1100 and
   400 wide. Not yet: a keyboard shortcut for "Clip the last"
8. **One live screen** (asked for 2026-11-15, mock `design/live-screen.html`, picked C and an
   always-on buffer): the preview says "LIVE · Records from now · the player opens, so you can
   clip as it happens" with **Watch**; **Record** records from now and opens the player on it
   ("Recording it live."). The player's bar: not recording → "Save the last ▾" + Record (Twitch
   and others that can't rewind keep the last 10 min by themselves, with a free-space check);
   recording → Start clip / Save the last ▾ / Stop; ended → "Saved · 0:24 · 9.6 MB · you
   stopped it" with Show file / Trim it / Record again (no Record again once the stream ended).
   **"Also get the 1 h 8 min before"** under the recording's line: `record::fetch_earlier` runs
   from the start next to it until past where it began (YouTube's piece count overshoots:
   the file is measured and yt-dlp resumes until it really has), then the join is found by
   the sound (Chromaprint on 25 s of the recording against 5 min of the earlier part, at 0 /
   30 / 60 s in, ≥ 0.8); saving cuts the earlier part there and puts it in front. Measured:
   the clock alone is 5–11 s off; Twitch starts a from-now recording with its own "Commercial
   break in progress" ad, which the match skips (30 s in). Checked headless (`--earlier-at`):
   osu! on Twitch, 1:05:13 before + 9:30 after, one file, picture continuous across the join.
   Not offered when the site keeps no past (Twitch subscriber-only broadcasts fail with the
   site's words). **Fixed on the way**: a Twitch from-now recording carried two frames shown at
   the stream's own clock where its ad ends (~31 s in), so the saved file claimed a wrong
   length (7878 s for 4517 s); saving, "Clip the last" and a part the stream ended now show
   any frame more than 5 s ahead of its decoding where it's decoded (`TS_FIX`, ffmpeg's
   `setts`): that part saves at 45.8 s, as long as its sound. A part the stream ended also
   gets its start left out now (`skip`), as the others did. The black picture while
   watching wasn't seen again; the same spot in the player is the likeliest cause.
9. **Twitch without its ads** (asked for 2026-11-15, the TTV-AB idea, see `docs/UPSTREAM.md`):
   `ttv.rs`, a relay on 127.0.0.1 that serves a channel's playlists from the Twitch players
   that come without the ad (a fresh `site` token opened on a pre-roll while `embed`,
   `popout`, `mobile_web` and `frontpage` had the stream at full quality), merged by each
   piece's date-time (every player lists the same 2 s pieces at the same times, so a switch
   has no seam). A from-now part and the player read it instead of twitch.tv; a stretch where
   every source has the ad is left out and goes on the tape as missed. Signed in to Twitch
   (Turbo, subscribers), or when the relay can't reach the stream, everything reads
   twitch.tv as before. Checked: recording and watching with `site` asked first
   (`NERIA_TTV_ORDER=site,embed,popout`) open on the stream, not "Commercial break in
   progress"; the 10-minute buffer reads the relay too. Not seen yet: a mid-roll (Twitch
   decides when), and Twitch giving the ad to every player at once.
10. **Missed bits, got back** (asked for 2026-11-16): where the connection dropped or slowed,
   the site still has that stretch, so `record::repair_holes` fetches each missed bit (20 s
   before it, 60 s after) once it's past and the recording runs steadily (the queue looks every
   30 s; a last pass before saving). YouTube while live: the last hour, placed by the clock
   (`live::section_now`); after it ends, the video; Twitch: the broadcast copy (from the start,
   cut). Saving splices it in where the sound says the recording broke off and picks up again
   (6 s windows of the fetched bit slid along the recording; fingerprints padded with silence
   where timestamps jump, so they're on the file's clock), and the hole leaves the tape: "got
   back 0:12 missed". Checked with a killed connection (`--kill-at`): YouTube, a 12 s hole by
   piece numbers, got back, 166.6 s picture and sound; Twitch without the relay, a 5 s hole
   by the clock, got back, and the ~28 s ad the reconnect brought went with it. Not got back:
   Twitch broadcasts for subscribers only, YouTube holes older than its hour while live, sites
   that keep nothing. Not yet checked: the background pass in the app (only the last pass). Not yet:
   the queue row's own "get the part before", and a crash while it's fetched (the earlier part
   is dropped, the recording is kept).
11. **Live chat** (asked for 2026-11-18, mock in the chat of that day): read-only, never
   signed in (`chat.rs`). Twitch live: its chat server anonymously; YouTube live: its own chat
   feed ("Live chat", not "Top chat"); past streams: YouTube's chat replay through yt-dlp
   (2 h, 43,740 messages in 1:52), Twitch's saved chat paged by second, 8 stretches side by
   side (its page cursors need a browser check; 3.3 h, 37,920 messages in 1:46). 7TV,
   BetterTTV and FrankerFaceZ emotes (their public lists, fetched beside the reading), emoji as
   Noto's pictures, bans and deletions taken out.
   **Saved with it**: a recording reads its chat all along (into its folder line by line, so a
   crash keeps it) and saves `Video.chat.json` timed to the file (checked: 42 s of Twitch,
   354 messages); a past stream downloaded whole gets its chat after the video ("Saving the
   chat…", a note if it can't). Settings → Download → "Save live chat with recordings and past
   streams" (on).
   **On the Clip page**: the chat beside the player, a full-height column from 1200 px,
   beside the player below that, a sheet on compact. It follows the player (live and a stream
   that can't be rewound: the newest), All / Highlights / Picked, find, the wheel and Page Up
   / Down go back, "Jump to live"; click a message to jump there (3 s before). Rows are drawn
   by `chat_draw` (the same drawing a burned-in clip gets). An activity strip over the
   timeline shows where chat got busy (yellow inside In–Out). A past stream's link offers
   "Load chat" (a quick look at its page says whether it kept one); a file opens with its
   `.chat.json`. "+" reads another stream's chat alongside (YouTube and Twitch together, a
   YT / TW mark on each).
   **Into the clip** (Chat in this clip): left out, as a track (styled in .mkv, plain lines in
   .mp4), or burned in beside the picture (under an upright one) or over it (the last lines,
   fading), dark or light; everything in the clip, highlights (Super Chats, subs, mods, your
   words) or the ones ticked; leave out bots and !commands; emotes as pictures. Files, link
   clips (through the queue, moved by however early a lossless cut starts) and clip lists
   (each its own, or joined in order). Checked: all three from the app (`NERIA_CHAT_EXPORT`)
   on a recording, and headless on a test video. Not yet: chat for "Save the last" / "Clip
   the last" while live; a from-the-start recording's chat before you pressed Record (load it
   from the past stream once it ends); Kick.
Dev hooks: `NERIA_TTV_ORDER` (the relay's players, in order), `NERIA_TTV_OFF` (no relay), `NERIA_DIALOG=live` / `live-upcoming` / `live-long` / `close-rec`, recording rows in
`NERIA_DEMO`, `NERIA_REC_STOP`, `NERIA_REC_DEBUG`; headless `--example record` (also
`--queue`, `--kill-at`, `--crash-at` + `--save`); ui-check `live`, `live-soon`, `live-long`,
`close-rec`, `chat`, `chat-dialog`, `chat-sheet`, `chat-file`, `watch-live-chat`; chat:
`NERIA_CHAT_DEMO`, `NERIA_CHAT_EXPORT`, `NERIA_CHAT_DEBUG`, `NERIA_CHAT_OFF`, `--example chat`.

## Capture: instant replay of a game or the screen (own track)
Prototype on branch `capture-prototype` (commit 009fc2e; engine `capture.rs`,
`capture_sound.rs`, `hotkey.rs`; headless `--example capture`). Medal / ShadowPlay-style: the
last minute is always kept, and saving turns it into a file that opens in the clipper.
1. [x] **Engine (prototype).** FFmpeg grabs on the GPU (`ddagrab` a monitor, `gfxcapture` one
   window) straight into the GPU's encoder; the route is tried at start (`Route::pick`:
   NVENC, AMF, Quick Sync, NVENC through memory, x264). Pieces that overwrite the oldest;
   saving includes the piece still being written
2. [x] **Sound (prototype).** WASAPI, a track each: "Everything" (the mix) first, the PC (or
   everything but one app), one app's process tree (Discord), a mic; silence filled by the
   clock
3. [x] **Hotkey (prototype).** `RegisterHotKey` (no hook, nothing for anti-cheat to object to),
   F8 by default, a "saved" sound
4. [x] **UI.** Built 2026-11-21 (mock `design/capture.html`, picks 1A · 2A · 3A · 4A): a
   Capture page in the rail (red dot while on): a plain status ("Keeping the last 1 min of
   VALORANT", "Waiting for VALORANT to open", why it couldn't), Turn on / Save now / Turn off;
   what to record by name (games first, other windows on request, monitors by their own name
   on every graphics chip: a laptop's second screen on the NVIDIA chip didn't record before);
   how much (30 s – 5 min); the save key (press it; one another program holds is refused);
   Turn on when Neria starts; the clips saved, with Open in clipper. A save plays the "saved"
   sound, Neria's toast when it's in front, else a Windows notification with Open in clipper.
   Tray: the status, Save the last, Turn on / off; closing while on goes to the tray. Voice:
   "clip that" saves while it runs, "undo that" deletes it.
   Then (user's ask, mock `design/capture-tracks.html`, picks 1A · 2A · 3A): **Automatic**
   (the game in front, else the screen in use; moves to the next game, not to the desktop),
   **a track per app** (track 1 Everything, then the game and each app playing sound, the
   mic), **Never record** (one app: left out of what the PC plays; two or more: Everything
   built app by app, a new app taken in at a quiet restart), and the clipper's Sound →
   **Tracks** (each on/off and 0–200%, mixed again into the first track, or kept apart too;
   heard in the player). Not in the mock's plan: an "Everything else" track (Windows can't
   subtract several apps at once), so an app that starts later is in Everything only until
   the next start. Left: resizing (1440p is kept as 1440p), a real game session
5. [ ] **"Hey Neria, clip that".** Listener prototype built 2026-11-21 (`voice.rs`,
   `--example voice`): the phrase is whatever the user types (default "clip that"), several
   allowed. Checked: live on the user's mic 6 of 6 heard, nothing in between, ~2–3% of one
   core; Windows' two speech voices 9 of 10 clean, 10 of 10 with moderate noise, 2 of 10 when
   the noise is louder than the voice; "clip it", "clippy", "clip the end" never fired.
   A made-up word as spelled ("Neria") wasn't heard; spelled as it sounds ("Nearia") it was,
   so the UI needs a **Test it**: say it a few times, Neria tries spellings and keeps what
   works. Built 2026-11-21 (mock `design/voice.html`, picked 1A · 2A · 3A): Settings → Voice
   (on/off fetching the model, phrases, mic + meter, Test it with three sayings and
   sound-alike spellings offered, Keep the last, the sound) and the live player (a Listening
   mark that stops it for this stream, a yellow flash, a "Heard" toast; Clip the last while
   recording with Undo, Save the last while watching). The phrase splitting was wrong until
   then (its check was skipped): fixed, and "neria clip that" is now heard as typed. Checked:
   screenshots at every width with sample data. Then, from the user's first real tries
   (2026-11-21): heard 3 of 3 in Test it and on a live stream; clips end where the player
   is (it runs behind the live edge), the full length rewound from the site when it keeps
   it, else what's kept; saving doesn't pause a live stream; Undo and Show while watching
   too; the mark's menu (Keep the last, Stop / Listen again, "Not listening"); "undo that"
   said; Sensitivity (Normal / High / Highest) that Test it raises when it misses someone (a
   friend's accent wasn't heard at Normal). Test it redone as guided turns ("Say it now",
   4 s each with a countdown and the mic's level; "no sound from this mic" said at once), and
   **voice samples** (`voice_match.rs`): the three turns are kept and a saying that sounds like
   them counts too, either one is enough. Checked on recorded voices: the phrase matched at
   0.8–7, every other line at 11+, and another voice saying "clip that" matched none. Only
   works for the phrase said with a short pause around it (the words model covers the rest).
   Each Test it adds a **named set** ("Me", "Jake"; up to 5, each forgotten on its own), so
   several people or ways of saying it all count. Test it no longer raises Sensitivity by
   itself (user's call: more false alarms for everyone); voice samples are the accent fix.
   Capture's save by voice: built with Capture 4. Not yet: the friend's accent re-tested with
   samples.
   Off until turned on; the mic is only listened to on the PC,
   nothing is sent anywhere. A small always-on wake-word listener (around 1% CPU, never the
   large whisper model) that presses the same save as the hotkey, with the same "saved"
   sound. Then, if wanted: a few words after the wake word read by a small whisper model
   ("clip the last two minutes", "mark that"). Pick the engine by reading the real upstream
   options (openWakeWord, sherpa-onnx keyword spotting, Porcupine's licence) and record it in
   `docs/UPSTREAM.md`. Must not fire on game sound or friends on Discord: listen to the mic
   track only, and test with a game playing loudly

- [x] **Settings → Capture, OBS-style (2026-11-26).** The user couldn't find the encoder
   (it was folded under Quality → Advanced). Encoder now has its own heading on the page,
   and Settings → Capture shows the same choices (keep, Quality, Encoder, sound, Never
   record, save key, turn on at start), both bound to the one `Capture` global. Also
   Settings → General → **Video work on** (Automatic / Graphics card / Processor, `gpu.rs`)
   for everything outside instant replay: clips cut exactly, Sound and Picture, GIFs as
   MP4, chat and captions burned in (these were processor-only before), Compress's starting
   tick, workflows and speech to text; a file the card refuses is made again on the
   processor. Not yet: checked on an AMD or Intel encoder (only NVIDIA here).
- [x] **File names: real dragging; Settings moves (2026-11-26).** A piece of the name lifts
   and follows the mouse, a bar shows where it lands, and a field dragged up from below
   lands where it's dropped (`insert-field`), not only at the end. The Settings drawer
   slides in from the right and out again, the dimming fades with it, a pill slides to the
   picked section, and a section fades in. Not yet: watched by the user in the real app
   (screenshots only show them at rest).

## Phone: mirror, record and clip your Android phone (own track)
Planned 2026-11-21. scrcpy's video is excellent, but getting it running (developer options,
adb, IPs and ports for wireless) is the hard part, and it stops at a plain window. Neria's
version: the phone as a **capture source**, feeding the clipper and Compress, like the
instant replay prototype (`capture.rs`). Not a general phone manager. A desktop feature, so
the Android hold doesn't apply. Comes before Capture's UI (user's call, 2026-11-21).
Tools: `adb` (platform-tools) and scrcpy (Apache 2.0) fetched into `bin/` like the others,
one pinned scrcpy version (its server and client protocol must match), updated on purpose.
1. [x] **Connecting, the real fix.** (built 2026-11-22: QR, pairing code, USB hint, steps noticed from mDNS, phones remembered in `phones.json`; adb on its own port 5038) Wireless first: Neria shows a QR code, the phone scans it
   in Developer options → Wireless debugging → "Pair device with QR code" (Android 11+,
   adb's mDNS pairing, as Android Studio does), and it's connected: no IP, port or code to
   type. Pairing code as the fallback; USB too. A guided one-time setup with pictures (turn on
   Developer options, then wireless or USB debugging) that notices each step is done. A device
   list that remembers phones and reconnects on its own. Plain problems: "Tap Allow on your
   phone", "Wireless debugging turned off (it does after a restart)", "This phone needs its
   maker's USB driver: use wireless instead"
2. [x] **Mirror, quick version.** (built: `phone::open_window`, now what Present uses) Neria starts scrcpy with the right flags: sound (Android
   11+), phone screen off while mirroring, stay awake, Record to the Videos folder, the
   finished file opening in the clipper. The mirror is scrcpy's own window for now
3. [x] **Mirror in Neria's own page.** (built: our own scrcpy client, mpv draws raw H.264, Matroska with the phone's times feeds the 2 s pieces; the buffer starts again when the phone turns) Only `scrcpy-server.jar`: pushed with adb, it streams
   H.264/H.265 and Opus over a socket; Neria decodes and draws it (low-latency mpv or
   FFmpeg), and sends touches, keys and scrolling back over scrcpy's control protocol. A
   Studio + Play page (`docs/DESIGN.md`) with Record, a screenshot, and **Clip the last 30 s**
   from a replay buffer shared with Capture
4. [x] **Sound only** (built: WASAPI speaker, volume, Record as M4A) (asked for 2026-11-21): the phone's sound on the PC with no picture
   (music, a call, a game's sound while the screen stays off): scrcpy's sound forwarding
   (Android 11+) without its video, a small "Playing from Pixel 7" strip with its volume and
   Stop; Record saves it as a sound file
5. [x] **Present the phone** (built: scrcpy's own window "Phone screen · Pixel 7", on top, show taps; with a frame, since scrcpy's borderless window can't be dragged) (asked for 2026-11-21): a window of its own with only the phone's
   screen in it (no Neria around it, no bars), named "Phone screen · Pixel 7" so it's easy to
   pick when sharing a window in Discord, Teams, Meet or OBS, or to put on a projector for a
   class. Keeps the phone's shape as it's resized, turns when the phone turns, can stay on
   top; optional: show taps (scrcpy's `--show-touches`) so people see what you press, and keep
   the phone awake while presenting. Opened from the mirror page ("Present in a window")
   - [x] With its sound (asked for 2026-11-22; mock `design/phone-present.html`): Present is
     a window of Neria's own now (`present_ui.rs`), drawing the picture Neria already gets;
     the sound plays from Neria, so a window share in Discord carries it. scrcpy's window was a
     second session: its sound stuttered (even over USB), closing it handed the sound back to
     the phone's speaker, and its picture was a second stream over the Wi-Fi. The page's
     picture steps aside meanwhile (a card, Bring it to front); Clip the last keeps its sound.
     Checked on the POCO over Wi-Fi: smooth sound, taps work, closing keeps the sound here.
     Not yet checked: a Discord viewer hearing the shared window
   - [x] One choice of where the phone's sound plays, for Mirror, Sound only and Present: this
     PC (everything; the phone quiet, its buttons set the level) / this PC and the phone (apps
     may refuse: Spotify does) / the phone only; Volume on this PC always; applied at once
   - [x] The screen comes back on after "Turn the phone's screen off" (HyperOS left the
     backlight at zero: the brightness setting is nudged a step and back)
   - [x] Cable and Wi-Fi hand over (asked for 2026-11-22): mirroring by cable carries on over
     Wi-Fi when the cable comes out; mirroring over Wi-Fi offers the cable when it's plugged in
     (asked, since switching starts again). Checked both ways on the POCO
6. [x] **Extras** (built 2026-11-23, not yet checked on a phone): the phone's camera as a
   source (Android 12+); drop an APK to install it, drop files to send them to the phone;
   copy and paste between PC and phone
Watch out: only one adb server runs per PC (port 5037); another adb at a different version
(Android Studio's) kills ours and the other way round: notice it and use theirs or say so
plainly (done: ours runs on 5038). Portable rule: checked 2026-11-22: on Windows adb keeps its
key and pairings in `<profile>\.android` and no variable moves them (`docs/UPSTREAM.md`), so
that's an exception, written only once the Phone tab is used.

## Later: bigger ideas (keep UX simple)
- **Browser extension (Chrome, Edge, Firefox)**, parked 2026-11-23 (see Up next 8; a
  Tampermonkey userscript talking to a loopback port with a per-install key is the free
  alternative if it comes back): a Neria button on video pages and posts, and
  "Download with Neria" when right-clicking a link, image or video. It hands the link to the
  desktop app (a `neria://` link handler, or a localhost port only Neria listens on), which
  comes to the front with the preview ready; "Download now" queues it with your defaults. Can
  also hand over the page's cookies for that one site ("Use my sign-in for this download"),
  which answers Chrome's locked cookies. Portable: registering the handler is a button in
  Settings, not an installer
- **Audio tools page**: **Convert** (MP3, M4A/AAC, Opus, FLAC, WAV; bitrate, mono/stereo,
  sample rate under Advanced; "Extract audio" from videos; batch). **Even out loudness**
  across songs. **Tag editor** (many files at once; "Find tags online" from Spotify).
  **Organize** (Artist / Album folders, M3U playlists, duplicates). Split by timestamps /
  remove silence. Mock `design/audio-tools.html`; picks chosen 2026-11-24
  (1B no sixth page, each tool where its nearest cousin is: Convert / Extract
  as a Video / Sound only switch on Compress (2A), Even out "as an album", Edit tags dialog
  with "Several" (3A), picking several Library rows with a bar of shared actions (4A), Put
  in folders with every move listed (5A), Duplicates as a Library filter, Paste times… and
  Cut silence on the Clip page). Organize is the one place files move (each move shown,
  Undo); the Library's "never moves files" rule gains that exception. ✅ Built 2026-11-24:
  Compress has Video / Sound only, sound goals as formats (Plays everywhere M4A, MP3,
  Smallest that sounds good Opus, Lossless FLAC, For editing WAV; `Goal::Format`), a
  converted sound named with its new extension, Even out "As an album" (`album_gain`), Cut
  the silence at the start and end (sound files only: a video's sound keeps time with its
  picture, so not on the Clip page's Sound); Edit tags (`tags.rs`, `tags_ui.rs`); picking
  several Library rows (tick on hover, Ctrl/Shift+click, Esc) with a strip of what fits them
  all (Convert to sound, Even out, Edit tags, Put in folders…, Delete files…, Copy files,
  Remove); Put in folders (`organize.rs`); a Duplicates tab with Pick the extra copies; the
  clip list's Paste times… (`tracks::parts_from_text`). Each tried in the app with the
  `NERIA_*` hooks on a scratch home (Sound only saved an .m4a, two files evened out as an
  album kept their 10 dB apart, Find tags online filled the fields and cover and Save wrote
  them, Move + Undo put files and the playlist back, silence cut 30 s to 20.4 s); ui-check
  at every width. Checked by hand 2026-11-24 after fixes: rows no longer blink on hover (the
  hover area holds the row's content), the pick tick has its own column on the left, songs
  show their own cover, edited tags retitle the row. Left: long-press to start picking on a touch screen (Android); Find tags online for several files at once; duplicates by sound
  (fingerprints) rather than by link
- **Library page** 🚧 (built 2026-11-23, mock `design/library.html`, picks 1A · 2A · 3A ·
  4A + 4B · 5A; screenshots checked at 1280 / 900 / 720 / 400): a fifth page after Capture (on compact,
  Settings moves to a gear in each page's header and the tab bar shows the five pages).
  Everything Neria saved, in `library.json` (`library.rs`): filled once from `history.json`,
  Capture's Saved list and the Spotify folders' manifests, then written by the queue (link
  clips, songs and captions only too), instant replays (PC / phone), clips from files,
  compressed copies, GIFs and transcripts. Calm rows by day with kind tabs (All, Videos,
  Sound, Clips, Recordings, Replays, Gone while any), Site / Date / Sort and search; a
  Spotify playlist is one row with its songs in its details. Details in a right column
  (wide), opened in place (medium), a sheet (compact); Watch / Play the one yellow action;
  Clip (Clip again of a file clip opens the original at In / Out), Compress, Transcribe,
  Make a GIF, Show in folder, Copy file, Download again, Copy link, Go to the original,
  Update playlist, Add lyrics, Remove, Delete file… (Recycle Bin). Gone checked in the
  background ("On E:, not connected" for a missing drive); Find it… also finds the other
  gone files from that folder; Find them… for a whole folder. Frames cached in
  `library-thumbs/`. Since 2026-11-23 (second pass): an open page shows each save at once
  (`library::on_saved`, no more 30 s re-read); a song's ⋯ has Pick another match (the
  Download page's dialog: its track from Spotify by the record's link, else from its tags and
  the folder's manifest; Use this queues the match over the file); a recording saved with
  holes keeps them (`record::Gaps`) and its ⋯ has Get the missed bits back
  (`record::get_holes_back`: fetched again, spliced in by sound, the rest kept); songs from
  the queue keep their Spotify link (was the YouTube Music match); Download again takes the
  Gone record's place; watching a saved file offers Show in folder, not Download. Tried in
  the real app (`NERIA_LIBRARY_ACT`, scratch home): Watch / Play and Back, Clip, Clip again
  at In / Out, Compress, Transcribe, Make a GIF, Copy file / Copy link (on the clipboard),
  Download again, Update playlist, Add lyrics, Go to the original, Pick another match + Use
  this, Get the missed bits back (a YouTube video with 10 s cut out: back, ~2.5 s overlap
  from keyframe cuts, as when recording), Remove, Delete file…. Checked by hand the same day:
  Find it… (its neighbours found too), Find them…, Show in folder, a copied file pasted into
  Explorer; Copy file is on every file now. Left: the grid view (2B) on video kinds; the Gone
  check with an unplugged drive (no drive to hand); Pick another match
  marks no row as the current one (the Library doesn't know which match it was), and a song
  saved before in another format than Settings' leaves the old file next to the new one;
  Twitch keeps a stream's missed bits only while it's live
- **Command palette** (Ctrl+K): type "compress", "extract audio", "clip"… to jump to any
  action; also lists recent files. Mock `design/palette.html`; picks chosen 2026-11-23:
  1A a box over the dimmed page, 2A empty box = copied link + 3 recent things + 3 common
  actions, 3A + 3B thing → its actions or action → which file, 4A "Do" above "Saved" (5
  each), 5A a rail button with Ctrl K / on compact a magnifier next to the gear and a sheet.
  ✅ Built 2026-11-24 (`palette_ui.rs`, `ui/dialogs/palette.slint`, matching in
  `neria_core::palette`, picks remembered in `palette.json`): 24 actions (pages, Settings
  sections, Clip / Compress / Make a GIF / Transcribe a file, instant replay on / off / save,
  the phone, Add lyrics, Follow, updates) found by their own words too, the Library's
  things with their ⋯ actions, a copied or typed link. Each Enter tried in the app
  (`NERIA_PALETTE` + `NERIA_PALETTE_ENTER`); ui-check `pal*` at every width; Ctrl+K and the
  keys checked by hand the same day. Left: on the Clip page at 400 px the header's four icons
  cut the title shorter once a file is open
- **Saved workflows** ✅ (built 2026-11-24, mock `design/workflows.html`, picks 1A · 2A · 3A ·
  4A · 5A · 6A): what happens to a file after it's saved (`workflow.rs`, `workflow_ui.rs`,
  `workflows.json`). One change (Make it smaller for Discord / email / WhatsApp / a size,
  Sound only as MP3 / M4A / Opus / FLAC / WAV, Make a GIF of a video up to a minute, or left
  as it is) then one place (next to the original, a folder, or replacing it: the original to
  the Recycle Bin). When: picked (a Then button beside Transcript under a video's preview),
  every download, links from ticked sites, new uploads of followed channels, or files put in a
  watched folder (taken once they stop growing, `.part` / `.crdownload` skipped, files already
  there left alone unless ticked, results in its Done folder); the more specific wins, never
  two on one file. Starters: Discord-ready, Sound only, Into a folder. Settings → Workflows
  (Edit, Pause / Resume an automatic one, New workflow), the editor says it all back in one
  sentence (or why Save is greyed out); Run a workflow… in a Library row's menu and the
  picked-rows strip; the palette finds a workflow by name, then which file. It runs on the
  download's own queue row ("Saving its sound as MP3… 40%", then "Sound only: MP3, 0.5 MB";
  Show opens the result); a step that fails leaves the download saved, with a note; a new file
  is its own Library record ("made by …", the original its source). Checked headless
  (`--example workflow`: smaller, already small enough, Sound only, GIF, moved, replaced, a
  watched folder) and in the app (an automatic Sound only on a real YouTube download, the
  row and both Library records); ui-check `set-wf`, `wf-*`, `pal-wf`, `lib-menu` at every
  width. Left: the Then menu and the editor's folder pickers clicked by hand (screenshots
  can't open a pop-up or a file dialog); a watched folder in the running app with a real
  OBS recording; a row whose workflow step failed says why but has no Try again for the step
  (the mock had one); a builder for chains (download → clip → 9:16 → compress) only if
  people ask
- **External tools** ✅ (built 2026-11-24, mock `design/tools.html`, picks 1A · 2A · 3A · 4A ·
  5A; instead of a plugin system): the user's own command-line programs as Neria actions
  (`tool.rs`, `tool_ui.rs`, `tools.json`). Settings → Tools (after Workflows): name, program
  (picked, a name found in Neria's `bin` or on PATH, or a whole command line pasted and split
  once), its words with blanks (`{file}` `{out}` `{dir}` `{name}` `{ext}`, each passed as a
  whole argument: no shell, so `a&b.mp4` can't run anything), It makes (a new file ending in…
  / nothing), It takes (videos / sound files / both), It'll run (the real command for a sample
  file), Try it on a file…. Runs from a Library row's menu and the picked-rows strip (Run a
  tool…, once there's a tool), the palette (by name, then which file), and as a workflow's
  Change it. One run per file, on its own queue row: the program's last line, the bar only
  when it prints a percentage (and only forward), no console window, Cancel ends its whole
  tree; a failure says the exit code (Windows' own in hex) and its last line, and Copy the
  report has the command and everything it printed; a new file goes in the Library. A tool
  runs by itself (every download, a watched folder) only after it has run once by hand and
  finished; changing what it runs asks for that again. Nothing comes with Neria, nothing is
  fetched or imported. Checked headless (`--example workflow -- <file> tool …`: a file named
  `a&b clip.mp4` through ffmpeg, a failing one with its report) and in the app (a tried tool
  as an every-download workflow on a real YouTube download: both files, the row); ui-check
  `tool-*`, `set-tools`, `pal-tool`, `wf-tool`. Left: Try it / Change… / Run a tool… clicked
  by hand (file pickers and the run dialog's pick aren't scripted); a `{files}` blank (all
  files in one run) only if someone asks; a .bat / .cmd program is run by Rust through
  cmd.exe with its arguments escaped (Rust refuses ones it can't escape safely)

## Android (on hold)
On hold until the desktop app has every feature we want; no Android work before then.
- Same `neria-core`; Slint Android UI (built on the adaptive layouts from the UI foundation);
  embedded Python for yt-dlp; ffmpeg + libmpv as `.so`
- **"Download with Neria" in the share sheet**: in YouTube, Instagram, X, TikTok, Spotify or
  the browser, Share → Neria opens Neria with the link pasted and the preview loading (the
  same picker for posts and playlists). "Download now" skips the app and queues it with your
  defaults, progress in a notification
- APK via GitHub / F-Droid (Play Store bans downloaders)
- **The phone as a capture source without adb** (talked about 2026-11-22): the Neria app on
  the phone shares its screen and sound to the desktop app (Android's screen sharing with
  one "Start sharing?" tap; no Developer options, no pairing with adb), feeding the same
  Phone tab: mirror, replay buffer, Clip the last, Present. Android won't let that route
  control the phone from the PC (no taps or typing), and apps may keep their sound out
  (Spotify does), so adb stays the way for control and every app's sound. `phone.rs` is
  already written to be shared with that app

## Not planned
- Zotify-style downloading of Spotify's own encrypted streams, or streamrip-style downloading
  from Qobuz/Tidal/Deezer accounts (DRM/protection circumvention; gets accounts banned)
- spotDL as a dependency: Neria already does its flow natively in Rust (see
  `docs/SPOTIFY.md` and `docs/UPSTREAM.md`); bundling Python would hurt portability and
  start-up
- Rewriting storage on SQLite / Tokio: JSON settings + manifests and threads are enough
  today; revisit only if the Library page needs it
- Downloading comments, subtitle translation through online services
