# Neria

A fast, portable desktop app to **download** videos and music, **clip** them, and **compress**
them. Built with Rust + Slint, powered by yt-dlp, FFmpeg and mpv.

- Download from YouTube, TikTok, X, Instagram, Twitch, Reddit, SoundCloud and 1,800+ other sites
- Trim any video or audio file on a real timeline, or clip a link *before* downloading it
- Shrink videos to fit Discord's 20 MB (or any size), using your graphics card when possible
- Paste Spotify links to get the songs from YouTube Music with full tags, cover art and lyrics

Windows 10/11 for now. Android is planned (see [ROADMAP.md](ROADMAP.md)).

---

## Getting it running

1. Download the latest `Neria-…-windows-x64.zip` from
   [Releases](https://github.com/PizzaSpark/neria/releases/latest).
2. Unzip it anywhere (a USB stick works too) and double-click `Neria.exe`.

Windows may say "Windows protected your PC" because the exe isn't signed yet: press
**More info → Run anyway**. Neria updates itself after that: when a new version is out, a
banner offers **Update now**.

### Or build it yourself

1. Install **Rust**: <https://rustup.rs> (pick the default options). You also need the
   **Visual Studio C++ build tools**, which the Rust installer offers to set up.
2. Get the code:

   ```bash
   git clone https://github.com/PizzaSpark/neria.git
   ```

3. Build it (the first build takes a few minutes):

   ```bash
   cd neria
   cargo build --release
   ```

4. Your app is `target\release\neria.exe`. Copy it to any folder you like (a USB stick
   works too) and double-click it.

### First start

The first time, Neria downloads the tools it needs into a `bin` folder next to the exe:
yt-dlp, FFmpeg, Deno (yt-dlp needs it for YouTube) and libmpv (the video player). That's
about 550 MB, once. After that it starts instantly and keeps yt-dlp updated by itself
(once a day).

Nothing is installed on your system. Neria keeps everything next to the exe:

```
neria.exe
settings.json      your settings
bin\               yt-dlp, ffmpeg, ffprobe, deno, libmpv
cookies\           sites you signed in to (only if you did)
```

Notifications are the one exception: Windows only shows them for an app it knows, so Neria adds
one entry for itself to your user's registry (its name and icon). Turning notifications off in
Settings removes it.

To move Neria, move the folder. To remove it, delete the folder.

---

## Using it

The sidebar on the left has three pages, **Download**, **Clip** and **Compress**, plus
**Settings** at the bottom.

### Download a video or song

1. Copy a link, then press **Paste** (or paste into the box).
2. A preview appears with the title, thumbnail, channel and length.
3. Pick what you want: **Best quality**, **Compatible** (an MP4 that plays everywhere),
   **Smallest file** or **Audio only**. **Advanced** has the resolution and the file type.
4. Press **Download** (or Enter).

More ways in:

- **Several links at once**: paste them (one per line), or drop a `.txt` with links. Each one
  is looked up, and you untick the ones you don't want.
- **Paste & download** (the ▾ next to Download, or Ctrl+Shift+V) skips the preview.
- **Watch it first**: the ▶ on the preview plays the video before you download it.
- **Follow** a channel or playlist (under its preview): its new uploads download by
  themselves. Manage them in Settings → Following.
- Playlists and posts list their items first: tick what you want, drag them into your own
  order. Videos with captions, other audio languages or chapters show a line to pick those.

The download shows up in the **Queue** below with its progress. When it's done, each row
has buttons to:

| Button | What it does |
|---|---|
| 📁 | Show the file in Explorer |
| ✂ | Open it in the clipper |
| ↙↗ | Open it in the compressor |
| ✕ | Remove it from the list (the file stays) |

Files are saved to **Videos\Neria** by default. Change that under **Settings → Download → Save to**.

### Download songs from Spotify

Paste a Spotify link: a song, album, playlist or artist. No Spotify account is needed.

1. The preview shows the cover, the number of songs and the total length.
2. Press **Download N songs**.

Each song is found on YouTube Music and saved with Spotify's title, artists, album, track
number, year and cover art, plus lyrics (inside the file and as a `.lrc` file next to it).
Albums and playlists get their own folder and a `.m3u8` playlist file.

Good to know:

- **Neria checks every song by sound** against an official 30-second preview (Spotify's,
  or Deezer's or Apple's). Verified songs show **✓ sounds like Spotify**.
- If a song **doesn't match** (for example only a re-recording exists on YouTube Music), Neria
  tries other versions automatically. If none fits, the row turns **amber**.
- Press the **⇄ swap button** on any Spotify row to **pick a different version**. The list
  shows every version with its album, length and score, and checks them by sound: the one
  marked **✓ Same recording** is the right one. Press **Use this** to replace the file.
- **Paste the same link again later** to get newly added songs. Songs you already have are
  skipped. If songs were removed from the playlist on Spotify, you get a **Review** button.
  Nothing is deleted unless you tick it, and ticked songs go to the Recycle Bin.
- Without your own Spotify key, playlists show their **first 100 songs** and artists their
  **top 10**. See [Spotify key](#optional-your-own-spotify-key) below for the full list.
- The audio comes from YouTube Music (about 128–160 kbps). Picking FLAC or WAV doesn't make
  it sound better.

### Clip (trim) a video or song

Open the **Clip** page, then either:

- press **Open file…** for something on your computer, or
- press **Paste link** to clip a YouTube (or other) link before downloading it, or
- press ✂ on a finished download.

Then:

1. Drag the purple **handles** on the timeline to set where the clip starts and ends.
   Click anywhere on the timeline to jump there. The white line is the **playhead** (where the
   video is right now), not a marker; only what's between the purple handles is your clip.
   Playing stops at your end handle, and pressing play there replays the clip from the start.
   Turn on **loop** (the ⟳ button or **R**) to hear the clip on repeat.
2. Choose **Fast · lossless** (instant, original quality) or **Exact · re-encode** (cuts on
   the exact frame).
3. Press **Export clip** (or **Download clip** for links).

The clip is saved next to the original, e.g. `video (clip 0m12s-0m45s).mp4`.

In Fast mode the cut can only start on a *keyframe*. The white ticks on the timeline show
where those are. If your start point is between them, the extra part is shaded amber, and
**Snap to keyframe** moves your start there. Use Exact mode to cut anywhere.

**Keyboard shortcuts**

| Key | Action |
|---|---|
| Space or K | Play / pause (stops at the end handle; at the end it replays the clip) |
| I / O | Set start / end at the current position |
| R | Loop the clip on / off |
| ← → or J / L | Jump 5 seconds (hold Shift for 1 second) |
| , / . | Step one frame back / forward |
| Home / End | Jump to the start / end of the clip |

### Compress a video

1. Open the **Compress** page and press **Open file…** (or press ↙↗ on a finished download).
2. Choose a goal:
   - **Fit a size**: 20 MB (Discord), 25, 50, 100 MB or **Custom**, or
   - **Pick a quality**: drag the slider between Smaller and Better.
3. Choose a codec: **H.264** plays everywhere, **H.265** is smaller, **AV1** is smallest.
4. Check the line under the options. It shows the plan, e.g. "Will fit in 20 MB · 1920×1080 ·
   30 fps · H.264 on Intel Quick Sync".
5. Press **Compress**.

Neria keeps the original resolution by default. Set **Output** to **Auto resolution** to let
it pick the sharpest resolution and frame rate the size can carry, or cap it at 1080p / 720p /
480p. It uses your
graphics card (NVIDIA, Intel or AMD) when it can, which is much faster. If a result comes out
too big, it automatically redoes it smaller, so it always fits.

The result is saved next to the original, e.g. `video (compressed 20MB).mp4`.

---

## Settings

Open **Settings** at the bottom of the sidebar (or the format button, e.g. "mp4", under a link
preview). The sections are listed on the left:

| Section | What's there |
|---|---|
| Download | Where files go (and the Audio / Video / Photos folders, numbering), video container (mp4 / mkv / webm), prefer H.264, subtitles, the audio format (mp3, m4a, opus, flac, wav; also used for Spotify songs), thumbnail and metadata, SponsorBlock, whole playlists, X GIFs, file names (built from pieces like Title and Channel), offering links you copy |
| Following | Channels and playlists you follow: how often they're checked, Check now, Pause, Unfollow |
| Spotify | Lyrics on/off, sound check on/off, the heads-up, your own Spotify key |
| Network | Sites you signed in to, cookies from your browser, speed limit, how many downloads run at once, extra yt-dlp options |
| General | What closing Neria does (keep going in the tray), notifications and their sounds, same as Windows / dark / light |
| About | Versions, updates for Neria and its tools, copy a report |

Clip and Compress have no settings: you pick everything per file on those pages.

### Signing in (private, age-restricted or members-only videos)

When a video needs a sign-in, its preview offers **Sign in to YouTube** (or the site it's on).
A small window opens; sign in there and press **Done**. Neria keeps that site's cookies in a
`cookies` folder next to it and only sends them to that site. Sign out under
**Settings → Network**. Keep in mind that anyone you give the Neria folder to gets your sign-in
too.

Under **Network → Cookies** you can also borrow a browser's sign-in instead (Firefox works
best; Chrome and Edge lock theirs).

### Optional: your own Spotify key

Only needed for playlists with more than 100 songs. Since 2026 Spotify only gives developer
keys to **Premium** accounts.

1. Go to <https://developer.spotify.com/dashboard> and log in.
2. Press **Create app**, give it any name, and set the redirect URI to `http://127.0.0.1`.
3. Copy the **Client ID** and **Client secret** into **Settings → Spotify**.

The key is stored only in `settings.json` next to Neria. If it stops working, Neria falls
back to the public page automatically.

---

## Troubleshooting

| Problem | Try |
|---|---|
| "Setup hit a snag" on first start | Check your internet connection and press **Try again** |
| "YouTube wants a sign-in" | Press **Sign in to YouTube** on the preview |
| A site stopped working | Restart Neria: yt-dlp updates itself once a day |
| A Spotify song is the wrong version | Press ⇄ on its row and pick the one marked ✓ Same recording |
| A Spotify song failed with "Pick one yourself" | Press ⇄ and search for it in the box at the top |
| The compressor's Compress button is greyed out | Wait a second: it's still checking your graphics card |
| Something else | Delete `settings.json` to reset settings, or delete `bin\` to re-download the tools |

---

## For developers

- [ROADMAP.md](ROADMAP.md): what's done and what's next
- [CLAUDE.md](CLAUDE.md): code layout, test commands and debug settings
- [docs/UPSTREAM.md](docs/UPSTREAM.md): the spotDL / Sunnify versions we studied, and how to
  check what changed since

```bash
cargo run -p neria
cargo test --workspace
```

Neria only downloads what yt-dlp can legitimately reach. Spotify songs come from YouTube Music,
never from Spotify's protected streams. Respect the terms of the sites you use and the rights
of the people who made the content.
