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
| HandBrake | [HandBrake/HandBrake](https://github.com/HandBrake/HandBrake) | `master` | `2ab11403ca9d44a1ea02252174153e0918fd5a09` (`preset/preset_builtin.json`) | 2026-10-29 | n/a | GPL-2.0: **numbers and ideas only, never copy code** (Neria is MIT) |
| TTV-AB | [GosuDRM/TTV-AB](https://github.com/GosuDRM/TTV-AB) | `main` | `1825fa5` | 2026-11-15 | 26.0.3 | MIT with repository attribution: code may be ported, crediting GosuDRM with a link to the repo |
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

### HandBrake (numbers and ideas only)
Studied for the Compress track (2026-11-15). What Neria's Compress takes from its built-in presets
(`preset/preset_builtin.json`), checked against `compress.rs`:
| Idea in Neria | HandBrake |
|---|---|
| "Smaller, still sharp" = H.264 CRF 22 at 1080p (its default preset, "Fast 1080p30") | `VideoQualitySlider` 22 |
| Smaller pictures get a slightly better quality number (−1 at 720p, −2 at 480p) | Fast 720p30 RF 21, 480p30 RF 20 |
| "As small as possible" = CRF 27 at 720p 30 fps | its "Social" presets' RF 27 ceiling |
| H.265 CRF ~2 under H.264's for the same look; AV1 (SVT) ~31–35 | x265 RF 22–24, SVT-AV1 30.5–34.5 |
| The size floor (≈0.035 bits per pixel) | "Social 25 MB 5 Minutes 360p60" = 500 kbps ≈ 0.036 bpp |
| "Too long: the first 1:40 would fit" (how long a size holds) | presets named by how long they fit ("1 Minute 720p60") |
| A slower encoder setting when the size is tight | Social 10 MB presets use x264 `slow`, 25 MB `medium` |
| Deinterlace only when the file is interlaced | every preset has Decomb on |
| Keep chapters, all sound tracks, captions; cut black bars; a live sample to watch | ChapterMarkers, audio/subtitle lists, auto crop, Preview window |
Not taken: the speed × resolution preset grid, RF numbers in the UI, anamorphic settings, burnt-in
"foreign audio" captions.

### Sunnify (ideas only)
| Idea in Neria | Upstream files to watch |
|---|---|
| Read Spotify metadata from public embed pages (no login, no API key) | `spotifydown_api.py`, `Spotify_Downloader.py` |
| Per-folder manifest: resume interrupted playlists, skip tracks already on disk | `Spotify_Downloader.py`, `sunnify_cli.py` |
| Spotify API/page health check | `scripts/check_api_status.py` |

### Lyrics sources (`crates/neria-core/src/lyrics.rs`)
Order: LRCLIB → Spotify's own synced lyrics (opt-in). Checked on 2026-11-14:

| Source | How Neria reaches it | State on 2026-11-14 |
|---|---|---|
| LRCLIB | `lrclib.net/api/get` (album + length) then `/api/search` (±3 s) | Works. Synced lyrics for all 13 songs of Random Access Memories ("Motherboard": instrumental), 49 of Today's Top Hits' 50 (the 50th has plain text only) and a "Spotify Singles" track. Rate limit: after ~14 quick requests it answers 429 with `Retry-After: 1`; Neria waits and asks again (before that fix, half a playlist read as "no lyrics found") |
| Spotify (Musixmatch) | The web player's way, with the user's own sign-in (below) | Works with a spare account: synced for 11 of Random Access Memories' 13, and "Raindance" (plain-only on LRCLIB); `providerDisplayName` is "Musixmatch"; ~0.2 s a song after one token |
| Genius | The search its own website uses (`genius.com/api/search/multi`, no token, as syncedlyrics does), then the song page's `data-lyrics-container` blocks (as spotDL's `genius.py` reads them) | **Blocked for Neria**: the same request gets 200 from curl but 403 from Neria's HTTP client (bot protection). Not worked around. Dropped (user's call); spotDL itself uses Genius's API with a token |
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
Python tool by the same author; was the source of a planned Perfect loop GIF in the clipper,
dropped on 2026-11-14 (user's call). Kept here in case it comes back.
Local checkout: `D:\Files\Documents\Code Projects\Active\Tools\gif-maker` (GitHub:
PizzaSpark/loopgif). Read `README.md` for the method and `loopgif.py` (~200 lines, ffmpeg +
numpy) for the details: frame fingerprints (64×36 grayscale, decoded through ffmpeg), seam
score over a few frames on both sides of a cut, static sections skipped, shorter cycle
preferred, `--from/--to` search range, `--rank` for the next-best loop, palettegen +
paletteuse (sierra2_4a) render. `legacy/` holds older OpenCV/SSIM experiments, not needed.
Port to Rust in `neria-core` (no Python in Neria).

### whisper.cpp (MIT; prebuilt, run as a tool: `transcribe.rs`)
ggml-org/whisper.cpp, its own Windows builds, fetched on demand like ffmpeg (never bundled):
`whisper-blas-bin-x64.zip` (CPU) or `whisper-bin-win-cuda-12.4.0-x64.zip` (NVIDIA; only
`whisper-cli.exe`, `whisper.dll`, `ggml*.dll`, cuBLAS/cudart are kept). The release tagged
"latest" (`v1.9.5`) carries no files; the builds hang off `bNNNN` releases, so Neria takes the
newest one listing the asset (`b5454` known good, the fallback). The CUDA 11.8 zip lacks
cuBLAS and silently runs on the CPU: don't use it. Models from Hugging Face:
`ggerganov/whisper.cpp` (`ggml-large-v3-turbo-q5_0.bin`) and `ggml-org/whisper-vad`
(`ggml-silero-v5.1.2.bin`). Flags we rely on: `-oj` (JSON: `result.language`,
`transcription[].offsets` in ms), `-pp` ("progress = N%" on stderr), `--vad -vm`, `--prompt`,
`-ng` (the CPU when the card can't). If an update breaks it, check whisper-cli's
`examples/cli/cli.cpp` for renamed flags first.

### Wake word for "Hey Neria, clip that" (`voice.rs`, prototype built 2026-11-21)
Baseline: sherpa-onnx crate `=2.0.2` (static Windows lib, its own build script fetches it;
the link warns LNK4098, LIBCMT vs the dynamic CRT, harmless so far) and the gigaspeech 3.3M
int8 model. The model ships no tokenizer for us: `Pieces` reads `bpe.model` (sentencepiece's
protobuf, pieces + scores) and, as a unigram model, picks the split whose scores add up
highest (not pairwise merges: that gave `▁W OR L D` for `▁WORLD`), checked against the model's
own `keywords_raw.txt` → `keywords.txt` (the test needs an absolute `NERIA_HOME` with the model). If phrases stop being heard after an update, compare
`text2token`'s output first.
- **sherpa-onnx keyword spotting (k2-fsa, Apache 2.0): the pick to prototype.** Open
  vocabulary: the phrase is plain text turned into the model's tokens (`text2token`), one line
  per phrase with a boost (`:1.5`) and a trigger threshold (`#0.35`), so "Hey Neria" needs no
  training. English model `sherpa-onnx-kws-zipformer-gigaspeech-3.3M-2024-01-01` (16 MB, a
  14 MB `-mobile` int8 one) on the `kws-models` release; sherpa-onnx itself was at v2.0.2
  (2026-11-19). Official Rust crate `sherpa-onnx` (`KeywordSpotter`, streaming; its build
  fetches a prebuilt static Windows x64 library, so no extra DLL next to the exe). Unknown
  until tried: how well an uncommon name like "Neria" spots, CPU while idle
- openWakeWord (dscripka): code Apache 2.0, but its pretrained models are CC BY-NC-SA, a
  custom phrase means training (Colab, synthetic speech, ~1 h) and its runtime is Python
  (only a community C++ port). The fallback if sherpa's spotting is too loose
- Porcupine (Picovoice): needs an AccessKey checked online and its console for custom words;
  not portable or offline enough for Neria
- whisper.cpp (already fetched for Speech to text): too heavy to listen all the time; at most
  reads a few words after the wake word

### ytarchive and streamlink (ideas only, for recording live streams: `record.rs`)

Read 2026-11-15: ytarchive `2110ce2` (2025-06-25, 0.5.0; `Info.go`, `util.go`,
`player_response.go`, `main.go`) and streamlink `5896d42` (2026-11-14, after 8.6.3;
`stream/hls/hls.py`, `stream/segmented/segmented.py`, `plugins/twitch.py`). Neria records
through yt-dlp, so these are references for behaviour, not code.

- **Where it starts**: ytarchive goes from `sq=0`, or the last 5 days (`LiveMaximumSeekable`,
  432000 s), as yt-dlp's `--live-from-start` does ("the last 120 hours"); streamlink starts 3
  segments behind the live edge (`hls-live-edge`), as ffmpeg's HLS reader does. That's why a
  reconnected from-now part overlaps the last one, and Neria trims it by piece number.
- **Missed pieces**: streamlink logs "Sequence gap of N segments" when a reload's numbers jump
  (pieces that left the playlist) and counts failed downloads apart ("Failed to fetch segment
  N"). Neria counts the same two from ffmpeg ("skipping N segments ahead", "Failed to open
  segment") and yt-dlp ("Skipping fragment"); never from ffmpeg's "Opening" lines, which skip
  pieces fetched on a kept-open connection. ytarchive writes strictly in order and never skips.
- **Dropped vs ended**: ytarchive retries a fragment 10 times, sleeping the target duration
  between, and forever while YouTube still says live; streamlink stops after no new segment for
  3× the target duration. Neria: a part that ends is followed by a `live_status` check, then a
  new part after 2/5/10/20/30 s, giving up after 5 quick failures.
- **Scheduled streams**: ytarchive sleeps to the scheduled time, then polls every 15 s and logs
  "Stream rescheduled" / "N seconds late"; Neria uses yt-dlp's `--wait-for-video 30-60`.
- **Worth doing later**: ytarchive's "a 404 within 2 pieces of the end is the end, not a hole";
  streamlink's Twitch ad detection (`EXT-X-DATERANGE` `CLASS="twitch-stitched-ad"`), to label
  ad breaks instead of counting them missed.

### Live chat (`chat.rs`, `chat_draw.rs`, `chat_out.rs`)

Built 2026-11-18 from what the sites themselves answer (checked by hand that day), not from
another project's code:
- **Twitch live**: `irc.chat.twitch.tv:6667`, `PASS SCHMOOPIIE` / `NICK justinfan<n>` (the
  anonymous login Twitch's own logged-out chat uses), `CAP REQ :twitch.tv/tags
  twitch.tv/commands`; emote places in the `emotes` tag are characters, not bytes.
- **YouTube live**: `youtube.com/live_chat?is_popout=1&v=<id>` gives `ytInitialData` with the
  view menu (its second item is "Live chat", everything) and the key and client version; then
  `youtubei/v1/live_chat/get_live_chat` with the continuation, waiting `timeoutMs`. Whether a
  past video kept its chat: `liveChatRenderer` on its watch page. New item kinds show up
  (`giftMessageViewModel` that day).
- **Twitch past broadcasts**: GraphQL `video.comments(contentOffsetSeconds:)` with the web
  client id; asking by cursor fails ("failed integrity check") and so does the persisted
  query, by second works.
- **YouTube replay**: yt-dlp's `--sub-langs live_chat` (`.live_chat.json`, one action a line,
  `videoOffsetTimeMsec`).
- **Emotes**: 7TV `7tv.io/v3/users/twitch/<id>` + `emote-sets/global`, BetterTTV
  `api.betterttv.net/3/cached/…`, FrankerFaceZ `api.frankerfacez.com/v1/room/id/<id>`; emoji
  as `fonts.gstatic.com/s/e/notoemoji/15.1/<codepoints>/72.png` (what YouTube's chat uses).

Worth reading when it breaks or for more: chat-downloader (xenova, MIT: YouTube and Twitch
item kinds), TwitchDownloader (lay295, MIT: its chat renderer, for animated emotes and badge
pictures), yt-dlp's `youtube` live chat code. Not read yet.

### TTV-AB (MIT with attribution)
| Feature in Neria | Upstream files to watch |
|---|---|
| Twitch without its ads (`ttv.rs`): the backup player types and their order, ad pieces known by "stitched" / not titled "live", Twitch's web client id and token query | `src/modules/constants.ts` (`PLAYER_TYPES`, `CLIENT_ID`, `AD_SIGNIFIER`), `src/modules/api.ts` (`_getToken`: the `PlaybackAccessToken` query, its hash, `platform` for `autoplay`), `src/modules/parser.ts` |

When Twitch ads come back in recordings: check whether TTV-AB changed its player types, the
query hash or how it spots ad pieces since the baseline, and ask for
a fresh `site` token (it opens on a pre-roll) and the others to see which still come without
(`--example record` with `NERIA_TTV_ORDER=site,embed` shows the switch). TTV-AB also rotates sources mid-break, holds a silent picture while it searches and
goes back to `site` after a break; Neria doesn't need those (it records, and a relay that
starts on a clean source stays on it).

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
| 2026-11-15 | TTV-AB | n/a → `1825fa5` | Idea ported for Twitch recordings and watching without ads (`ttv.rs`, credit there): ask Twitch for the stream as other players (`embed`, `popout`, `mobile_web`, `frontpage`), which came without the pre-roll `site` got; merge their playlists by each piece's date-time. Our own Rust relay; no code copied. |
| 2026-11-14 | spotify-lyrics-api | `e158b96` (still latest) | Now the idea behind the opt-in "Spotify's own synced lyrics" (user asked for it, with a spare-account warning). Our own Rust; secret read from Spotify's script, not from spot-secrets-go. No code copied. |
| 2026-11-18 | whisper.cpp | n/a → `b5454` | Speech to text (`transcribe.rs`): its prebuilt `whisper-cli` + ggml models, run as a tool. No code copied; JSON output parsed. |
