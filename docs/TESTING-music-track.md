# Music track: what to test by hand

Everything here builds and passes `cargo test -p neria-core` and `cargo test -p neria --test
ui_rules`, and was looked at in `scripts/ui-check.ps1` screenshots (`set-spotify`,
`set-lyrics`, `set-lyrics-in`, `dl-spotify`, at every width). Tick items off as you go.

## Checked headless on real links (2026-11-14)
With `--example lyrics` and `--example spotify` (new flags: `--lyrics-only`, `--number`,
`--add-lyrics`, `--no-lyrics`, `--no-verify`, `--no-spotify-preview`, `--spotify-lyrics`).
- LRCLIB: synced lyrics for all 13 songs of Random Access Memories ("Motherboard" says
  "instrumental, no lyrics"), each lookup 0.3–0.5 s; 49 of Today's Top Hits' 50, twice in a
  row (Dave's "Raindance" has plain text only).
- Synced lyrics only: "Raindance" says "no synced lyrics found"; all sources off: "no
  lyrics found".
- In the window (dev hooks `NERIA_LYRICS_ONLY`, `NERIA_ADD_LYRICS`): "Lyrics only" on the
  album made a "Lyrics · Random Access Memories" group of 13 rows with the `.lrc` names and
  "synced lyrics from LRCLIB"; "Add lyrics" on a folder of 3 songs (lyrics stripped) put
  `.lrc` files next to them and the words back inside.
- Lyrics only (4 songs of that album): `01 Daft Punk - Give Life Back to Music.lrc` + `.txt`
  etc., in the album's folder, no audio.
- Today's Top Hits, first 3 songs: downloaded unnumbered, then again with Number playlist
  songs on and Add lyrics: the three files were renamed `01 …`, `02 …`, `03 …` ("Already
  downloaded", nothing fetched again), the M3U points at the new names, each got a synced
  `.lrc` and the words inside the mp3 (`lyrics` tag).
- No preview anywhere: "Tappat - Spotify Singles" (A36) with Spotify's preview set aside:
  Deezer and Apple find nothing (also true for two other Spotify Singles tracks), the song
  downloads and the row says "no preview to check against · synced lyrics from LRCLIB".
- Spotify's own lyrics, token step only: the secret is read from Spotify's web-player script
  (version 61) and an anonymous token comes back, so the TOTP is right.

Bugs found and fixed on the way:
- The web-player script has other `{secret:` objects (its TOTP library) before the real
  list, so the first one wasn't it.
- LRCLIB's rate limit (429 after ~14 quick requests) read as "no lyrics found" for half a
  playlist: now waited out.
- Lyrics rows offered Clip and Compress on `.lrc` files.
- Narrow windows (under 720 wide): "Paste" and "Download 13 songs" squeezed the link box down
  to "http". Download now has its own full-width row under the box and Paste is the box's
  icon. The Settings tabs cut "About" off at 400 wide: they fit now.

Not covered headless: the real Spotify lyrics call (needs your spare account), anything in
the window.

## 1. Spotify's own synced lyrics (needs a spare Spotify account)
- [ ] Settings → Spotify → Lyrics: tick "Spotify's own synced lyrics". The "Use a spare
      account" tag and the warning show before anything else; then **Sign in to Spotify…**.
- [ ] The sign-in window opens on Spotify's login; after signing in the bar says "Signed in to
      Spotify", Done closes it, the toast says "Neria uses it for lyrics only", and Settings
      shows "Signed in to Spotify · Sign out".
- [ ] `cookies/spotify.com.txt` holds only spotify.com cookies.
- [ ] Headless: `--example lyrics -- --only spotify --show <a track LRCLIB has no synced
      lyrics for>` shows "synced lyrics from Musixmatch via Spotify" and real LRC lines. (A
      song only on Spotify, or a new release, is a good bet.)
- [ ] Download such a song: its row says "synced lyrics from Musixmatch via Spotify" and the
      `.lrc` plays in sync (e.g. in a player that shows LRC).
- [ ] Sign out (Settings → Spotify, or → Network): the next song says "Spotify lyrics are on
      but you're not signed in to Spotify" in brackets after its lyrics status.
- [ ] A cookie that ran out (sign out on spotify.com in a browser): the row says "Spotify
      sign-in ran out: sign in again in Settings → Spotify", once per session.
- [ ] The cookie and token never show in a copied report (`Copy report` on a failed row).

## 2. Lyrics settings and rows
- [ ] Untick LRCLIB: songs fall through to the next source; untick all: rows say "no lyrics
      found".
- [ ] Synced lyrics only: a song with only plain lyrics says "no synced lyrics found" and gets
      no lyrics tag.
- [ ] Turning "Add lyrics" off hides the source ticks, and rows say nothing about lyrics.
- [ ] Genius: blocked for Neria (403), so it's off and its tick hidden. Decide what to do
      with it (ROADMAP Music 1).
- [ ] Narrow window (400–719 wide): the link box has room, Download sits under it, its menu
      opens under the button; Paste (icon) still pastes.

## 3. Lyrics only and Add lyrics, in the window
- [ ] Paste a Spotify album: "Lyrics only" sits next to "Saved as mp3 …"; it's also in the
      Download ▾ menu. Pressing it adds a "Lyrics · <album>" group; the preview stays and
      "Download N songs" still works afterwards.
- [ ] Rows read "Looking for lyrics…", then the `.lrc` name with "synced lyrics from LRCLIB";
      a song with none says "No lyrics found" (amber). Running it again: "Already
      downloaded" for songs that have a `.lrc`.
- [ ] Rows of lyrics jobs have no "pick another match" button.
- [ ] Settings → Spotify → "Add lyrics to songs I already have…": starts in the Audio folder;
      picking a playlist folder shows a toast with how many songs, how many already have a
      `.lrc`, how many have no tags. A folder of songs not from Neria (tagged) works too.
- [ ] A FLAC / Opus / m4a song gets the words inside it too (tag names differ by format).

## 4. Number playlist songs
- [ ] Tick it, paste a playlist you downloaded before: nothing downloads again, the files get
      `01 `… names, `.lrc` files move with them, the M3U follows.
- [ ] Untick it, paste again: the names go back.
- [ ] A playlist of 100+ songs (with your own key) gets three digits: `001 …`.
- [ ] Albums stay numbered by track number either way; single songs are never numbered.
- [ ] Removed-songs review still finds songs removed from a numbered playlist.

## 5. Still untested from before
- [ ] Own-key Web API path (needs a real Client ID/secret from developer.spotify.com).
- [ ] Spotify playlists over 100 songs without a key: only the notice, no workaround.
