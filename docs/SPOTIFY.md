# Spotify links: how it works

Detailed notes for the Spotify flow (`spotify.rs`, `ytmusic.rs`, `music.rs`, `fingerprint.rs`,
desktop `spotify_ui.rs`). Status and next steps live in `ROADMAP.md`; where to look when
upstream breaks things is in `docs/UPSTREAM.md`.

Spotify audio itself is DRM-protected, so Neria does what spotDL does: read the
track/album/playlist metadata from Spotify, find the matching song on YouTube Music, download it
with the bundled yt-dlp, and tag it with Spotify's metadata.

Tested on Random Access Memories (13/13 matched at 1.00, whole album in 83 s) and All Out 80s.

## Links and metadata
- Paste a Spotify track / album / playlist / artist link (incl. `intl-xx` and `spotify:` URIs):
  preview with cover, song count, length; "Download N songs" queues one row per song, in order
- Metadata like Sunnify: public embed pages, no login, no API key. Limits shown to the user:
  playlists show the first 100 songs, artists their top 10, playlist tracks have no album name
  (filled from YouTube Music). Optional **own** Client ID/secret in Settings → Spotify
  uses the Web API (full playlists, ISRC, track/disc numbers) and falls back to embed pages

## Matching
- Like spotDL: YouTube Music "Songs" search through its web API (~0.6 s per song), ISRC query
  first when known, scored on title + artist + duration (±3 s full marks, >30 s rejected) with
  penalties for live/remix/cover/re-recorded/… versions the Spotify title doesn't mention.
  Matches below 0.8 are downloaded but flagged "Check match: …" in amber; below 0.6 the song
  fails instead of downloading the wrong thing
- "Pick another match" (swap button on Spotify rows, amber when flagged or failed): lists the
  YouTube Music candidates with album, length difference and match %, marks the current one,
  free-text search, open-in-browser to listen, and "Use this" re-downloads in place
  (`--force-overwrites`) keeping Spotify's tags, cover and lyrics. Songs with no confident match
  fail with "Pick one yourself" instead of silently skipping

## Audio check
- `fingerprint.rs`, on by default, Settings → Spotify: after each download, Spotify's
  public 30 s preview (`audioPreview.url` in the embed data) and the file are fingerprinted with
  ffmpeg's Chromaprint muxer; the preview is slid along the song for the best bit agreement.
  Measured: same master 0.98, same recording edited 0.95, same recording other master/length
  0.76, re-recording 0.58, different song 0.57. Verdicts: ≥ 0.72 "✓ sounds like Spotify",
  0.66–0.72 "close, maybe another master", below that a different recording → automatically
  tries up to two other good candidates (skipping other copies of a version that already
  failed), then goes back to the closest and flags it "Doesn't sound like the Spotify preview
  (58% similar). Pick another?". ~1 s per song
- Reference preview fallbacks: Spotify's preview first, then Deezer's public API (exact ISRC,
  else search), then Apple's iTunes Search API. A fallback clip is only trusted if its title,
  artist and length (±3 s) match Spotify's, so a re-recording can't become the reference.
  Rows say which preview they matched ("✓ matches the Deezer preview"); with none anywhere they
  say "no preview to check against". Measured with Deezer as reference: original 0.88,
  re-recording 0.64 (cut-offs 0.72 same / 0.66 unsure)
- Picker sound checks: when the dialog opens it finds the reference and checks the top 3
  matches plus same-title versions ruled out by length (often the album version of the same
  recording), in parallel; every other row has a "Check" button. YouTube refuses plain ffmpeg
  reads, so yt-dlp fetches the candidate's audio to the cache (~10 s each, deleted after).
  "✓ Same recording" rows float to the top. On Bonnie Tyler this surfaces the 6:58 album
  original (76%) above the re-recordings

## Tags, files, lyrics
- Tags: title, artists, album artist, album, track n/total, disc, date, ISRC, 640px Spotify
  cover; YouTube description/URL noise removed. Lyrics from LRCLIB: embedded (USLT) + synced
  `.lrc` next to the file (toggle in Settings)
- Album tracks named `01 Artist - Title`, playlists `Artist - Title`, one folder per
  album/playlist, `<name>.m3u8` in order
- Output note: source is YouTube Music (~128–160 kbps Opus/AAC); FLAC/WAV don't add quality

## Resume and sync
- Resume like Sunnify: re-pasting a link skips songs already on disk ("Already downloaded")
- Sync like spotDL, but safe: `.neria-sync.json` manifest per folder; songs no longer on the
  Spotify list show a "Review N removed" dialog, all unticked by default, ticked ones go to the
  Recycle Bin (only for complete lists, never for truncated ones)

## Risks
- Spotify keeps tightening its Web API (Feb 2026: Development Mode needs Premium, 5-user cap,
  fewer endpoints), and YouTube Music's web API isn't official; both can break, see
  `docs/UPSTREAM.md` for where to look first

## References and decisions
- Sunnify (sunnypatell/sunnify-spotify-downloader) does exactly this flow (public embed pages,
  no login; YouTube match on title + artist + duration). Its code is under a custom
  educational-use license, so Neria reimplements the approach rather than copying code
- Lyrics (`lyrics.rs`): LRCLIB → Spotify's own synced lyrics (Musixmatch, opt-in, the
  user's own Spotify sign-in, the idea of akashrchandran/spotify-lyrics-api, GPL so ideas
  only) → Genius. Off by default, with a "use a spare account" warning: it's against
  Spotify's terms and risks the account. Lyrics only, never audio. Details, and why
  Musixmatch without an account and AZLyrics aren't used: `docs/UPSTREAM.md` → Lyrics sources
- "Lyrics only" (a Spotify preview) and "Add lyrics to songs I already have" (Settings →
  Spotify) are `music::Mode` jobs in the queue: no matching, no download
- "Number playlist songs" (off by default): `01 Artist - Title`; when it or the order
  changes, `music::rename_moved` renames files by Spotify id from the old manifest
- Not planned: Zotify-style downloading of Spotify's own encrypted streams, or streamrip-style
  downloading from Qobuz/Tidal/Deezer accounts (DRM/protection circumvention; gets accounts
  banned). SoundCloud already works through yt-dlp
