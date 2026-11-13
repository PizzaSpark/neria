# Download track: what to test by hand

Everything here builds and passes the automated checks (`cargo test -p neria-core`,
`cargo test -p neria --test ui_rules`, fmt, clippy) and was looked at in `scripts/ui-check.ps1`
screenshots, but none of it has been run against real sites yet. Tick items off as you go.
Design choices: `design/download-track.html` (all the recommended options were picked).

## Checked headless on real links (2026-11-13)
With `--example smoke` (it now takes the Download page's picks as `NERIA_*` variables, see
its header) and `--example follow --try-new`, on "Me at the zoo" (3 chapters, en/de captions)
and MrBeast's "$456,000 Squid Game" (24 audio languages, first 30 s). Ticked below where
that covers the whole line. Bugs this found, all fixed:
- Chapters as one file kept only one piece (each piece was named "NA" and overwrote the
  last): now named by where they start. 1+3 of the zoo video → one 7 s file (5 + 2).
- Captions "In the video" left the `.srt` files next to it, and never got into a webm
  (yt-dlp skips SRT there): files removed afterwards, VTT for webm.
- Captions only left the video's thumbnail (`.webp`) behind.
- Note: "Both" into a webm saves the caption files as VTT too, even with SRT picked (one
  conversion serves both).
- Smallest file picked a Vietnamese dub (smallest stream first): every preset now prefers
  the video's own language. Compatible kept English AAC too.
Also seen working: SRT files (en + de), text, embedding into mp4 (mov_text, eng/deu), Audio
only in two languages with a `%(uploader)s/` folder template (`Audio\MrBeast\… (Spanish).mp3`),
Compatible = H.264 + AAC with English and Spanish tracks, Smallest ≤ 480p, and a full follow
check (one new upload found, downloaded to `Audio\Kurzgesagt – In a Nutshell\`, seen
afterwards, the next check finds nothing). Not covered headless: anything that needs the
window, signing in, VLC's menus, Separate chapter files (built in the desktop app).

## 1. Private and signed-in downloads
- [ ] Age-restricted YouTube video: the preview says "This video is age-restricted" with
      **Sign in to YouTube**. The window opens on Google's sign-in, the bar turns to
      "Signed in to YouTube" after signing in, Done closes it, the link is looked up again
      and downloads.
- [ ] `cookies/youtube.com.txt` next to the exe holds only `youtube.com` cookies (no
      `google.com` ones). `webview/youtube.com/` holds the window's browser profile.
- [ ] Members-only / private-shared-with-you video with an account that has access.
- [ ] Instagram post after hitting the anonymous limit → Sign in to Instagram → works.
- [ ] Your own YouTube "Liked videos" / "Watch later" playlists after signing in.
- [ ] Closing the sign-in window without signing in: toast "Not signed in…", nothing kept.
- [ ] Settings → Network → Signed in: the site shows with "signed in today"; **Sign out**
      removes it (and the next sign-in starts signed out). "A site" box + Sign in for a site
      typed by hand (e.g. vimeo.com).
- [ ] Import a `cookies.txt` exported by a browser extension with cookies of several sites:
      one file per site appears; a link of one of those sites downloads.
- [ ] A site that is signed in but still fails (account without access): button says
      "Sign in again to …".
- [ ] Spotify links and sites you're not signed in to still work as before; the browser
      cookies setting still works for sites without a Neria sign-in.
- [ ] Release build (`windows_subsystem = "windows"`): the sign-in window opens without a
      console window.
- [ ] Three signed-in YouTube downloads at once: all work, and `cookies/youtube.com.txt`
      keeps its date (each run uses its own copy in `cache/cookie-runs/`, since yt-dlp
      writes the file back when it ends; checked by hand that it does).
- [ ] A signed-in download a few days after signing in (YouTube rotates its session
      cookies; the copies don't feed the rotated ones back), and again after "Sign in
      again".

## 2. Captions
- [ ] A YouTube video with creator captions: "Captions: …" shows under the chips; the
      picker lists "Made by the creator" then "Auto-generated", the video's language first.
- [ ] Search (more than 8 languages) filters; All/None act on what the search shows.
- [ ] SRT + Separate: `Title [id].en.srt` next to the video. VTT and ASS likewise.
- [x] Text: a `.txt` with just the words (no numbers/timings), no `.srt` left behind.
- [ ] In the video (mp4 and mkv): VLC's Subtitle menu lists every ticked language; no
      caption files left next to it. Both: the menu *and* the files.
- [ ] Two auto-generated languages (e.g. English (Original) + a translation) download
      (YouTube rate-limits translated captions sometimes: say what happens).
- [ ] Captions only: no video, the row shows the caption file; "Show" finds it.
- [ ] The picked languages are ticked again on the next video that has them.
- [ ] A YouTube video without captions says "No captions on this video".
- [ ] Audio only + captions: files are saved next to the mp3 (nothing to embed into).

## 3. Audio tracks
- [ ] A YouTube video with dubbed tracks (e.g. MrBeast uploads): "Audio: Original (…)"
      shows; the picker lists every language with "Original" marked.
- [ ] Original + Spanish, mp4: one file; VLC's Audio → Audio Track menu shows both, named
      by language. Same with mkv.
      (Headless: the mp4 has English and Spanish AAC tracks, tagged eng/spa. Still open:
      VLC's track menu, and mkv.)
- [ ] Only Spanish ticked: the file has just the Spanish track.
- [ ] Audio only + two languages: two rows in one group, `… (Spanish).mp3` and
      `… (English).mp3`.
- [ ] A video with one audio language shows no Audio button.
- [ ] Descriptive audio tracks (if a video has them) are listed with "(descriptive)".

## 4. Chapters
- [ ] A YouTube video with chapters: "Chapters: all N"; untouched it downloads the whole
      video as before.
- [ ] One file, chapters 2+3: one file, starts and ends at the chapter edges (named with
      "(clip …)").
- [ ] One file, chapters 2+3+5: the line says "skips chapter 4"; one `… (chapters).mp4`
      that plays 2→3→5 without a glitch at the joins; no `.neria-part` files left.
      (Headless: chapters 1+3 joined into one 7 s file. Still open: 2+3+5, the "skips"
      line in the window, glitches at the joins.)
- [ ] Separate files, video: a folder named after the video with `01 Intro.mp4`, … each
      starting near its chapter (keyframe cut), title tag = chapter title.
- [ ] Separate files + Audio only on a full-album upload: `01 Song.mp3`… with title and
      track number tags; the group card counts them.
- [ ] Separate files + captions: one captions file in the same folder.
- [ ] Clipper: open a downloaded file with chapters (yt-dlp embeds them with "embed
      metadata" on): the strip under the timeline; clicking a chapter sets In/Out and
      seeks; the picked one is filled. "Clip first" from a preview with chapters shows
      them too.

## 5. Tray, notifications, Download later
- [ ] Close with a download going: the question; "Keep going in the tray" hides the
      window, a tray icon appears, a one-time toast says so; the download finishes.
- [ ] Tray: left click (and double click) shows the window again; right click menu: Show
      Neria, Pause all / Resume all (follows the queue), Quit Neria (queue saved, comes back
      next start).
- [ ] "Don't ask again" + either answer: Settings → General shows it; closing next time
      does it without asking. Closing with nothing going just quits.
- [ ] Toast on a finished download while Neria isn't focused: title, thumbnail, "Open"
      and "Show in folder" work; clicking the toast body shows Neria. Shows "Neria" as the
      app name with the Neria icon (registry key above). Failed download: "Couldn't
      download it" with the error.
- [ ] A 12-song album: one toast "12 songs saved" (not 12 toasts). With a failure:
      "11 of 12 saved, 1 failed".
- [ ] Clip export and compress: "Clip saved" / "Compressed" / "Couldn't compress it".
- [ ] "Only when I'm not looking at Neria": no toast while Neria is the focused window.
- [ ] Sounds: Neria chime (up), low chime on failure (down), Windows' sound, none; picking
      one in Settings plays it.
- [ ] Turning notifications off removes the registry key
      (`reg query HKCU\Software\Classes\AppUserModelId\Neria.Downloader` fails).
- [ ] Download ▾ → Download later… → Tonight, 02:00: the row says "Later · Starts at
      02:00" with Start now; it starts by itself at 02:00 (Neria in the tray). "At a time"
      with 22.30 and 22:30; a bad time says "Type a time like 22:30".
- [ ] A scheduled row survives a restart (still waits); Start now starts it at once.
- [ ] A playlist downloaded later: every item waits; the card's Start now starts all.
- [ ] Only starts if the PC is awake at that time (no wake timer): with the PC asleep at
      02:00 it starts when it wakes. Decide whether the note should say so.

## 6. Picker: order, Reverse, format per item
- [ ] A YouTube playlist: drag a row by ≡ up/down; a line shows where it lands; numbers
      follow. Tab to a handle, ↑ / ↓ moves the row.
- [ ] Reverse flips the list (numbers follow); Download: files are numbered in your order
      (`01 …` is the one you put first) and the `.m3u8` lists them that way.
- [ ] Paste the same playlist again: it opens in your order; new videos come after.
- [ ] A row set to "Audio · m4a" saves an .m4a while the others stay video; "Best · mkv"
      saves that one as .mkv.
- [ ] Thumbnails still land on the right rows after moving rows while they load.

## 7. Format presets
- [ ] Best quality: as before (container and "Up to" from Advanced).
- [ ] Compatible: an MP4 with H.264 video and AAC sound (check in the file's properties or
      `ffprobe`); plays in Windows' Photos/Films app.
      (Headless: H.264 + AAC. Still open: plays in Windows' Photos/Films.)
- [ ] Smallest file: an MP4 at 480p or less, noticeably smaller than Best.
      (Headless: 480p, original language. Still open: compared with Best's size.)
- [ ] Audio only: the audio format from Advanced (mp3 by default).
- [ ] An old `settings.json` with `"quality": "audio"` opens with Audio only picked.
- [ ] Clip first on a link: the same chips; the clip downloads with the picked preset.

## 8. Filename builder
- [ ] Settings → Download → File names says "Title [Video ID]" (the old default) and
      Change opens the builder showing Title, " [", Video ID, "]".
- [ ] Each ready-made name: the line and example change; download a video, the file is
      named like the example. "Playlist / 01 Title" with "Download whole playlists" on puts
      a playlist in its own folder, numbered.
- [ ] Click and drag fields up onto the line; drag pieces left/right; ←/→ and Delete on a
      focused piece; × removes.
- [ ] Add text " - ", and "/" (makes a folder): the example shows it.
- [ ] Warnings: typing ":" or "?" in text; Title alone (could collide); a line without
      fields; a "/" at the start or end.
- [ ] + Save mine: it appears as a chip, survives a restart, picking it brings it back.
- [ ] Advanced: type a template with an unknown part (e.g. `%(view_count)s`) + Enter: the
      builder says it's custom; picking a ready-made name goes back to pieces.
- [ ] With a link previewed, the example uses its title.

## Combinations (each feature was checked on its own)
- [x] Compatible preset + two audio languages: the audio is still AAC (the language
      selectors may beat the preset's codec choice).
- [x] Captions "In the video" + webm: does the file have captions? (yt-dlp only embeds
      VTT into webm.) ASS in mp4 becomes plain mov_text (styling lost).
- [ ] A file name with a folder ("Playlist / 01 Title") + Audio only with two languages,
      and + Separate chapters: where do " (Spanish)" and "03 Chapter" end up?
- [ ] Smallest file + chapters as One file (forced keyframes under 480p).
- [ ] Captions only, then paste the link again: no "You downloaded this" (fixed: captions
      aren't recorded as the video).

## 9. Several links, Paste & download, clipboard watcher
- [ ] Paste three YouTube links on separate lines: "3 links", each row fills in its title,
      length and picture; Download makes three rows (not a playlist card), in that order.
- [ ] Ctrl+V of lines into the box (they may arrive glued together): still split.
- [ ] A link already in the queue / downloaded before / pasted twice (`&list=` and plain):
      unticked with "In the queue" / "Downloaded before" / "Twice in this list".
- [ ] A playlist link and a Spotify link among them: "Paste it on its own", unticked.
- [ ] Drop a `.txt` with links (one per line): the same list. A `.txt` without links:
      "There are no links in that file."
- [ ] Download ▾ → Paste & download with two links on the clipboard: two rows start, no
      preview; toast says so. Ctrl+Shift+V in the link box does the same (and doesn't
      paste the text). Only a Spotify link: it goes into the box with its preview.
- [ ] Settings → Download → "Offer links I copy in other apps": copy a link in the
      browser → the strip; Download / Preview / ✕ work. Copying the same link again doesn't
      offer it twice; copying in Neria (the link box, Copy report) doesn't offer anything.
- [ ] In the tray with the watcher on: a notification "Link copied"; clicking it shows
      Neria with the strip. Off: nothing, and no clipboard reads (no CPU use).

## 10. Queue order
- [ ] Two links waiting and one running: the waiting ones read top-down in the order they
      start; a new link lands below the last waiting row (not on top, as before).
- [ ] Drag a waiting row's ≡ up past another: "Up 1" shows while dragging; it starts
      first. Running and finished rows have no ≡ and stay where they are.
- [ ] Tab to a ≡, press ↓ twice: the row moves twice (the focus stays on it).
      Ctrl+↑ / Ctrl+↓: top / bottom. Same in the playlist picker.
- [ ] ⋯ → Move to top / Move to bottom (wide and compact; compact has no ≡).
- [ ] A waiting playlist card moves as one block; inside it, waiting songs move among
      themselves. The card keeps its ≡ while its rows report progress.
- [ ] The order survives a restart (`queue.json` is written in that order).

## 11. Watch before downloading
- [ ] A YouTube video's preview has the yellow play mark (not playlists, posts, Spotify,
      GIFs, a list of links). Tab reaches it, Enter starts it.
- [ ] "Watch it first.": plays by itself, no In/Out handles or buttons, Space / ← → / M
      work, I and O do nothing. Volume slider and mute change the sound.
- [ ] Download: back on the Download page, the video is queued with the chips' choice.
- [ ] Clip this: In/Out appear (whole video), Download clip works as with Clip first.
- [ ] A file was open in the clipper with In/Out set: watch a link, Back → the file is
      back with its In/Out. Clip first on a link, then watch the same link, Back → still
      open for clipping.
- [ ] Leave the Clip page while it plays (Download tab): the sound stops.

## 12. Follow a channel or playlist
- [ ] Paste `https://www.youtube.com/@kurzgesagt`: the preview lists videos (not "Videos"
      and "Shorts"); "Follow" under the picker. Follow → toast, button says "Following".
      (Headless: the preview lists 256 videos and is followable. Still open: the Follow
      button, its toast and "Following" in the window.)
- [ ] Settings → Following: the channel, "daily · checked just now". Check now: nothing
      new (only uploads after following count).
- [ ] Edit `following.json`: remove the newest link from `seen`, Check now: one card
      "Kurzgesagt · 1 new", saved in `Video\Kurzgesagt – In a Nutshell\`. Next check:
      nothing new (it's pending, then seen once downloaded).
      (Headless with `--example follow --try-new`: found the one new upload, saved it as
      audio in `Audio\Kurzgesagt - In a Nutshell\`, next check found nothing. Still
      open: the card in the window, and video in `Video\...`.)
- [ ] Delete that file and check again: it isn't downloaded again.
- [ ] Hourly / Weekly, Pause (no checks; "paused"), Resume, Unfollow (gone; the preview's
      button says Follow again).
- [ ] A playlist: Follow, add a video to it on YouTube, Check now: that one downloads.
- [ ] Close Neria for longer than the interval, start it: the missed check runs about 20 s
      after the tools are ready. With Neria in the tray, checks still run.
- [ ] Offline at check time: "couldn't check: …" in the list; next check works again.
- [ ] Audio only picked when following: new uploads come as audio, in `Audio\<name>\`.
