# Download track: what to test by hand

Everything here builds and passes the automated checks (`cargo test -p neria-core`,
`cargo test -p neria --test ui_rules`, fmt, clippy) and was looked at in `scripts/ui-check.ps1`
screenshots, but none of it has been run against real sites yet. Tick items off as you go.
Design choices: `design/download-track.html` (all the recommended options were picked).

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

## 2. Captions
- [ ] A YouTube video with creator captions: "Captions: …" shows under the chips; the
      picker lists "Made by the creator" then "Auto-generated", the video's language first.
- [ ] Search (more than 8 languages) filters; All/None act on what the search shows.
- [ ] SRT + Separate: `Title [id].en.srt` next to the video. VTT and ASS likewise.
- [ ] Text: a `.txt` with just the words (no numbers/timings), no `.srt` left behind.
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
- [ ] Smallest file: an MP4 at 480p or less, noticeably smaller than Best.
- [ ] Audio only: the audio format from Advanced (mp3 by default).
- [ ] An old `settings.json` with `"quality": "audio"` opens with Audio only picked.
- [ ] Clip first on a link: the same chips; the clip downloads with the picked preset.
