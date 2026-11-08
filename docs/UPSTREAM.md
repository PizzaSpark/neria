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

### spotify-lyrics-api (ideas only, not used)
PHP, not Rust. Gets Spotify's own (Musixmatch) line-synced lyrics from the web player's private
`spclient.wg.spotify.com/color-lyrics/v2/track/<id>` endpoint. Needs the user's `sp_dc` login
cookie plus a TOTP made from secrets pulled out of Spotify's web-player JS (it downloads them at
runtime from a third-party repo, xyloflake/spot-secrets-go). Its README says it's probably against
Spotify's terms. Not used: Neria's lyrics stay on LRCLIB (no login). Revisit only if we decide on
an opt-in "Sign in to Spotify for lyrics". Files to watch: `src/Spotify.php`.

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
