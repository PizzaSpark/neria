# Upstream references

Neria doesn't bundle spotDL or Sunnify; it reimplements their best ideas. This file records
**which upstream commit we last studied**, so when something breaks (Spotify changes its pages,
YouTube Music changes search, matching gets worse) we can look at what upstream changed **since
that commit** and port the fix instead of debugging from scratch.

After porting anything (or re-reviewing upstream), move the baseline forward and add a log entry.

## Baselines

| Project | Repo | Branch | Baseline commit | Date | Release | License |
|---|---|---|---|---|---|---|
| spotDL | [spotDL/spotify-downloader](https://github.com/spotDL/spotify-downloader) | `master` | `58ced9af69dce17d33ff280bafbcc917a9dba0aa` | 2026-10-26 | v4.6.0 | MIT: code may be ported, with credit in the source |
| Sunnify | [sunnypatell/sunnify-spotify-downloader](https://github.com/sunnypatell/sunnify-spotify-downloader) | `main` | `0138cbf107395057bac7e33d820a28837cbe8522` | 2026-11-02 | v2.6.0 | Custom educational-use: **ideas only, never copy code** |
| spotify-lyrics-api | [akashrchandran/spotify-lyrics-api](https://github.com/akashrchandran/spotify-lyrics-api) | `main` | `e158b96b854778d8ff51a3c9544ebb4f68b64907` | 2025-12-30 | n/a | GPL-3.0: **ideas only, never copy code** (Neria is MIT) |
| syncedlyrics | [moehmeni/syncedlyrics](https://github.com/moehmeni/syncedlyrics) | `main` | `3c8a318d9a8df26855bdc3a5d23f7fd2b99ade2e` | 2024-07-28 | n/a | MIT: code may be ported, with credit in the source |
| loopgif (our own) | [PizzaSpark/loopgif](https://github.com/PizzaSpark/loopgif) | `main` | `cf5c980ee120026feeaef370d4affba5023425ef` | 2026-11-07 | n/a | Ours: port freely |

Why Sunnify is ideas-only: its LICENSE allows using and redistributing the software only
"not modified or altered in any way", free of charge and with attribution, and it asks people
to contact the author about modification. Porting its code into Neria would be a modification. If
the author grants permission, record it here (date + link) and it can be ported like spotDL.

## What we take from each, and where it lives upstream

### spotDL (MIT)
| Feature in Neria | Upstream files to watch |
|---|---|
| YouTube Music search + match scoring (title / artist / duration / ISRC) | `spotdl/providers/audio/ytmusic.py`, `spotdl/providers/audio/youtube.py`, `spotdl/providers/audio/base.py`, `spotdl/utils/matching.py`, `spotdl/utils/search.py` |
| Synced lyrics (.lrc) + plain lyrics fallback | `spotdl/providers/lyrics/synced.py`, `genius.py`, `musixmatch.py`, `azlyrics.py` |
| Tags (title, artists, album, track no., year, cover, lyrics, ISRC) | `spotdl/utils/metadata.py` |
| Playlist sync (add new, remove deleted) | `spotdl/console/sync.py` |
| M3U playlist files | `spotdl/utils/m3u.py` |

### Sunnify (ideas only)
| Idea in Neria | Upstream files to watch |
|---|---|
| Read Spotify metadata from public embed pages (no login, no API key) | `spotifydown_api.py`, `Spotify_Downloader.py` |
| Per-folder manifest: resume interrupted playlists, skip tracks already on disk | `Spotify_Downloader.py`, `sunnify_cli.py` |
| Spotify API/page health check | `scripts/check_api_status.py` |

### Lyrics sources (`crates/neria-core/src/lyrics.rs`)
Order: LRCLIB → Spotify's own synced lyrics (opt-in) → Genius. Checked on 2026-11-14:

| Source | How Neria reaches it | State on 2026-11-14 |
|---|---|---|
| LRCLIB | `lrclib.net/api/get` (album + length) then `/api/search` (±3 s), with a `Neria/<version>` User-Agent as its docs ask | Works. Synced lyrics for all 13 songs of Random Access Memories ("Motherboard": instrumental), 49 of Today's Top Hits' 50 (the 50th has plain text only) and a "Spotify Singles" track. Rate limit: after ~14 quick requests it answers 429 with `Retry-After: 1`; Neria waits and asks again (before that fix, half a playlist read as "no lyrics found") |
| Spotify (Musixmatch) | The web player's way, with the user's own sign-in (below) | Token step works (an anonymous token comes back); the lyrics call itself still needs a real sign-in to confirm |
| Genius | The search its own website uses (`genius.com/api/search/multi`, no token, as syncedlyrics does), then the song page's `data-lyrics-container` blocks (as spotDL's `genius.py` reads them) | **Blocked for Neria**: the same request gets 200 from curl but 403 from Neria's HTTP client (bot protection). Not worked around. The code and its tests stay; spotDL itself uses Genius's API with a token |
| Musixmatch without an account | syncedlyrics' way: `apic-desktop.musixmatch.com/ws/1.1/` with `app_id=web-desktop-app-v1.0` and the token from `token.get` | **Not usable**: `token.get` gives an all-zero token. With it, `track.search` still finds the right song (Get Lucky, id 147905071), but `macro.subtitles.get` (by name or by Spotify id) returned an unrelated song ("NOKIA", Drake) with made-up lines: decoys for anonymous clients. Fetching subtitles by the real track id wasn't tried (our tooling stopped further probing of an endpoint meant for Musixmatch's own app). Not used |
| Musixmatch website | spotDL's `musixmatch.py` reads the lyrics page | Plain text only (never synced), and scraping their pages is what their terms forbid most clearly. Not used |
| AZLyrics | spotDL's `azlyrics.py` (`geo.js` → `x` code → search) | **Blocked**: every request, `geo.js` included, is redirected to `b.azlyrics.com` "request for access". Not used |
| NetEase | syncedlyrics' `netease.py` | Needs cookies hard-coded in that library. Not used |

#### Spotify's own synced lyrics (opt-in, idea from spotify-lyrics-api)
spotify-lyrics-api is PHP and GPL: ideas only, no code. What Neria does, all in `lyrics.rs`
(`spotify_lyrics`):
1. Reads the TOTP secret from Spotify's **own** web-player script
   (`open.spotifycdn.com/cdn/build/web-player/web-player.<hash>.js`, linked from
   `open.spotify.com`): the first `{secret:'…',version:N}` in the list (the script uses
   `[0]`). spotify-lyrics-api downloads it from a third-party repo (xyloflake/spot-secrets-go)
   instead; Neria never does that.
2. Key = each character's code XOR `(i % 33) + 9`, the numbers written one after another,
   those digits' bytes. TOTP = RFC 6238, HMAC-SHA1, 30 s, 6 digits (`totp`/`totpServer` from
   local and `open.spotify.com/api/server-time`, `totpVer` = N).
3. `open.spotify.com/api/token?reason=init&productType=web-player&…` with the user's `sp_dc`
   cookie (from Neria's own sign-in window, `cookies/spotify.com.txt`). `isAnonymous: true`
   means the sign-in ran out.
4. `spclient.wg.spotify.com/color-lyrics/v2/track/<id>?format=json&market=from_token` with
   `app-platform: WebPlayer`. `LINE_SYNCED` lines → LRC; `providerDisplayName` names the source.

Guard rails: off by default; Settings shows the warning (against Spotify's terms, the account
can be restricted or banned, use a spare account) before the sign-in button; only asked when
LRCLIB has no synced lyrics; one secret + token per session, in memory, never logged; a 401,
403 or 429 switches the source off until Neria restarts or you sign in again, and the row
says why. Never used for audio, metadata or playlists.

**Terms risk** (our reading of Spotify's terms and of how Musixmatch licenses lyrics; the
documents weren't re-read on 2026-11-14): Spotify's terms forbid accessing the service by automated means outside its
apps, and lyrics are licensed to Spotify by Musixmatch for its apps only. The risk is to the
user's account (restriction/ban), hence opt-in + spare account. Neria sends one token request
per session and one lyrics request per song that LRCLIB couldn't sync.

**When it breaks:** "Spotify changed its web player" on a row = the `{secret:` list moved or
`totpVerExpired` came back. Look at the web-player script for `totpVer` and the `secret:`
array, and at spotify-lyrics-api / spot-secrets-go's recent commits for what changed.
Files to watch: spotify-lyrics-api `src/Spotify.php`; syncedlyrics `providers/musixmatch.py`,
`providers/genius.py`.

### loopgif (our own, for "Perfect loop GIF")
Python tool by the same author; the source of the planned Perfect loop GIF in the clipper.
Local checkout: `D:\Files\Documents\Code Projects\Active\Tools\gif-maker` (GitHub:
PizzaSpark/loopgif). Read `README.md` for the method and `loopgif.py` (~200 lines, ffmpeg +
numpy) for the details: frame fingerprints (64×36 grayscale, decoded through ffmpeg), seam
score over a few frames on both sides of a cut, static sections skipped, shorter cycle
preferred, `--from/--to` search range, `--rank` for the next-best loop, palettegen +
paletteuse (sierra2_4a) render. `legacy/` holds older OpenCV/SSIM experiments, not needed.
Port to Rust in `neria-core` (no Python in Neria).

## How to check what changed since the baseline

Quick look in the browser (replace the SHA with the baseline above):

- spotDL: `https://github.com/spotDL/spotify-downloader/compare/58ced9af69dce17d33ff280bafbcc917a9dba0aa...master`
- Sunnify: `https://github.com/sunnypatell/sunnify-spotify-downloader/compare/0138cbf107395057bac7e33d820a28837cbe8522...main`

Focused log for just the files we care about (clones into `upstream/`, which is git-ignored):

```bash
git clone --filter=blob:none https://github.com/spotDL/spotify-downloader upstream/spotdl
git -C upstream/spotdl log --oneline 58ced9af69dce17d33ff280bafbcc917a9dba0aa..origin/master -- spotdl/providers spotdl/utils/matching.py spotdl/utils/search.py spotdl/utils/metadata.py spotdl/console/sync.py spotdl/utils/m3u.py
```

```bash
git clone --filter=blob:none https://github.com/sunnypatell/sunnify-spotify-downloader upstream/sunnify
git -C upstream/sunnify log --oneline 0138cbf107395057bac7e33d820a28837cbe8522..origin/main -- spotifydown_api.py Spotify_Downloader.py sunnify_cli.py scripts/check_api_status.py
```

- spotify-lyrics-api: `https://github.com/akashrchandran/spotify-lyrics-api/compare/e158b96b854778d8ff51a3c9544ebb4f68b64907...main`

Also skim both repos' open issues for the symptom (e.g. "429", "embed", "no match").

## Review log

| Date | Project | From → To | What we looked at / ported |
|---|---|---|---|
| 2026-11-07 | spotDL | n/a → `58ced9a` | Initial study for the phase 5 design (matching, lyrics, sync, M3U). Nothing ported yet. |
| 2026-11-07 | Sunnify | n/a → `0138cbf` | Initial study (no-login embed pages, resume manifest). Nothing ported yet. |
| 2026-11-07 | both | (baselines unchanged) | Phase 5 built. Our own Rust code: embed `__NEXT_DATA__` parsing (`spotify.rs`), YouTube Music `youtubei/v1/search` with the ytmusicapi "Songs" filter param (`ytmusic.rs`), scoring modelled on spotDL's ideas (title/artist/duration + version penalties), LRCLIB for lyrics. No upstream code copied. If matching or metadata breaks, check spotDL `spotdl/providers/audio/ytmusic.py` + `utils/matching.py` and Sunnify's embed parsing first. |
| 2026-11-08 | spotify-lyrics-api | n/a → `e158b96` | Studied for a lyrics-only mode. Needs `sp_dc` login cookie + TOTP secrets scraped from Spotify's web player; GPL-3.0. Not used; lyrics stay on LRCLIB. Nothing ported. |
| 2026-11-08 | loopgif | n/a → `cf5c980` | Recorded as the source for Perfect loop GIF. Nothing ported yet. |
| 2026-11-14 | spotDL | `58ced9a` (still latest) | Lyrics providers re-read for the Music track: `genius.py` needs an API token (Neria uses the website's search instead), `azlyrics.py` (site now blocks), `musixmatch.py` (page scraping, plain only), `synced.py` (wraps syncedlyrics). Genius page reading ported (credit in `lyrics.rs`). |
| 2026-11-14 | syncedlyrics | n/a → `3c8a318` | Studied: Musixmatch desktop API (now serves decoys anonymously, not used), Genius tokenless search (ported idea, credit in `lyrics.rs`), NetEase (hard-coded cookies, not used). Latest commit is from 2024. |
| 2026-11-14 | spotify-lyrics-api | `e158b96` (still latest) | Now the idea behind the opt-in "Spotify's own synced lyrics" (user asked for it, with a spare-account warning). Our own Rust; secret read from Spotify's script, not from spot-secrets-go. No code copied. |
