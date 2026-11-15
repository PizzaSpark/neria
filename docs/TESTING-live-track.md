# Testing the Livestreams track

What was checked on real streams (2026-11-15) and what still needs a check. Headless runs use
`NERIA_HOME=target/debug cargo run -p neria-core --example record -- <link> <now|start> <secs> [out]`
(see its header); app runs used a scratch `NERIA_HOME` with `NERIA_URL` + `NERIA_URL_START`
and `NERIA_REC_STOP`, windows on the second screen.

## Checked

| What | How | Result |
| --- | --- | --- |
| From now, YouTube | Sky News, in the app, Stop at 75 s | Saved 1:14, 1080p H.264 + AAC, plays; folder removed |
| From now, Twitch | `record` + `--queue`, Stop at 35 s | Saved; "Clip the last 10 s" made its own row in 0.2 s |
| From the start, YouTube | `record start`, lofi radio | Catches up ~6×; killed yt-dlp resumed the same part (`.ytdl`), no hole |
| From the start, Twitch | `record start`, shroud | Catches up ~12× from the VOD playlist (EVENT type, keeps growing) |
| Parts of both kinds join | from-start part + from-now part, `-c copy` concat | Plays across the seam, no decode errors |
| Reconnect, numbered pieces | YouTube from now, `--kill-at 20` | New part, overlap (~8 s) trimmed, no false hole |
| Stream ends by itself | local HLS stream (ffmpeg `-t 50` + http.server) | "the stream ended", yt-dlp's finished file saved |
| A missed piece | same, one segment deleted as it appeared | "missed a moment" (ffmpeg's "Failed to open segment") |
| Crash | Neria killed mid-recording, started again | No yt-dlp/ffmpeg left (job object); saved at start, "saved after Neria closed" |
| Closing (quit set) | WM_CLOSE while recording | Stopped, saved, exited in ~1 s |
| From the start, picked by itself | in the app, a YouTube stream 73 min old | "catching up: 10:07 of 1:16:49 · live in ~13 min"; Stop saved 15:05 |
| Clip the last, from-start pair | `record start --clip 10@30` | 10.0 s, video and sound start together, decodes clean (was 1:35 before the fix) |
| Clip the last, in the app | `NERIA_REC_CLIP=10@90` on the stream above | Its own Saved row |
| Watch + Start clip / End clip | in the app (`NERIA_REC_WATCH`, `NERIA_REC_MARK=30,70`), from the row and the player, 1100 and 400 wide | 50 s and 45 s clips (10 s lead), the recordings went on and saved |
| Pause all | queue test | Leaves recordings alone (it used to throw them away) |
| Layout | ui-check `dl`, `live`, `live-soon`, `live-long`, `close-rec` at 1280 / 900 / 720 / 400 | Fits; at 720 the line under the tape cuts its end ("catching up: 51:10 of 2:14:0…") and the row's speed shortens to "5.9 …" |

## Known gaps

- **Links that skip the preview** ("Paste & download", several links at once): a live link
  there isn't known to be live, so it goes the old way (yt-dlp records it as a download; Cancel
  deletes it, no Stop, no tape).
- Two "missed" bits once showed on a recording whose file had no gap: yt-dlp's skipped pieces
  near the newest one are now held back until both streams are 2 pieces past them (ytarchive's
  rule); not yet seen again on a real stream either way.

## Not yet checked

- Start clip / End clip across a dropped connection (the clip is cut from the newest part, so
  it's shortened to what that part holds), and while watching at 400 px the "Clipping from…"
  line is hidden (End clip shows a clip is open).

- A real **upcoming** stream or premiere: the "SOON · Starts in…" preview and the waiting row
  are only seen with sample data; `--wait-for-video` then recording from the first second, and
  a stream that starts late (rescheduled), are untested.
- **YouTube with rewinding off** asked for "from the start": should fall back to from now with
  "couldn't go back to the start" (code path not hit on a real stream).
- **Twitch from the start all the way to live** (a long catch-up reaching the live edge).
- Clicking in the window: "Record from now instead", "Clip the last ▾", Stop, the ⋯ →
  "Cancel recording…" question, the close question's buttons (each covered headless or by a
  dev hook, not by a real click).
- A long recording (hours): memory, the size counter, "Clip the last 10 min" on a big file,
  saving a multi-GB recording (time it takes).
- The free-space stop ("stopped: D: is almost full") with `NERIA_FAKE_FREE_MB`.
- Audio-only recordings (the Audio chip): saved as the audio format setting.
- Light theme.
