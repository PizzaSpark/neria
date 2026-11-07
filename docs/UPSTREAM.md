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

Also skim both repos' open issues for the symptom (e.g. "429", "embed", "no match").

## Review log

| Date | Project | From → To | What we looked at / ported |
|---|---|---|---|
| 2026-11-07 | spotDL | n/a → `58ced9a` | Initial study for the phase 5 design (matching, lyrics, sync, M3U). Nothing ported yet. |
| 2026-11-07 | Sunnify | n/a → `0138cbf` | Initial study (no-login embed pages, resume manifest). Nothing ported yet. |
| 2026-11-07 | both | (baselines unchanged) | Phase 5 built. Our own Rust code: embed `__NEXT_DATA__` parsing (`spotify.rs`), YouTube Music `youtubei/v1/search` with the ytmusicapi "Songs" filter param (`ytmusic.rs`), scoring modelled on spotDL's ideas (title/artist/duration + version penalties), LRCLIB for lyrics. No upstream code copied. If matching or metadata breaks, check spotDL `spotdl/providers/audio/ytmusic.py` + `utils/matching.py` and Sunnify's embed parsing first. |
