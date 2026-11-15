# Compress track: what to test by hand

Everything here builds and passes `cargo test -p neria-core` and `cargo test -p neria --test
ui_rules`, and was looked at in `scripts/ui-check.ps1` screenshots (`cmp-empty`, `cmp`,
`cmp-film`, `cmp-long`, `cmp-trim`, `cmp-sharp`, `cmp-many`, at 400 / 720 / 900 / 1000 / 1280
wide). Tick items off as you go.

## Checked headless on real files (2026-11-15)
With `--example compress` (new: goal names, `--range`, `--first-only`, `--bars`, `--preview`,
`--sample`) on made-up files: a 10 min 640×360 test video, a 40–60 s letterboxed film
(1920×800 in 1920×1080, two sound tracks, text captions, chapters), a phone video stored
sideways (1920×1080 turned 90°), an interlaced 576i file. This machine's graphics card: Intel
Quick Sync (H.264, H.265).
- Discord on the 10 min file: 360p, 19.8 MB, first try. 12 MB on it: the graphics card came out
  12.3 MB, the redo on the processor 11.9 MB ("redoing it on the processor for an exact size").
- The film for Discord with black bars cut: 1920×800, both sound tracks, the captions (as
  mov_text) and both chapters kept, 19.4 MB.
- The phone video: 720×1280 out (was squashed before: scaled as if landscape).
- The interlaced file: deinterlaced, 25 fps kept (a first try doubled it to 50).
- Trim 0:12–0:30 of the film: 18 s out, chapters moved to 0:00–0:08 and 0:08–0:18.
- Preview: "Smaller, still sharp" on the 10 min file estimated about 75.6 MB, the real encode
  73.9 MB; the before/after frames line up (same moment, bars cut on both).
- In the window: a folder of 5 videos with WhatsApp and `NERIA_COMPRESS_START`: 3 compressed
  (15.4 / 16.0 / 15.9 MB), the 2 already under 16 MB skipped.
- Posted mouse messages (the scratch `click-check.ps1` way, windows on the other screen):
  Trim opens the strip on the whole video and its frames; dragging the end grip to 0:28 sets
  "Compress 0:28" and the hints; the before/after line follows a drag; Cancel during a list
  keeps the finished file, marks the running one Cancelled (no half file) and offers the rest.
- `NERIA_PAGE=compress` + `NERIA_DROP=<folder>`: the folder's 8 videos listed.
- Light theme: the slider's tags, its grip and the trim grips stay light over the video.

## Known
- Captions and chapters in the same .mp4: ffmpeg's QuickTime chapter track comes out broken
  ("Referenced QT chapter track not found"). The chapters are still there for VLC, mpv and
  Windows' players; Apple's players won't show them.
- The upload limits are what was found on 2026-11-15 (see ROADMAP Compress 1). WhatsApp's
  is the lowest figure sources give (16 MB); it may take more now.

## To do by hand
- [ ] Open a real phone video, a real film (with black bars), a real screen recording: do the
      hints ("720p, a bit softer") match what you see in the result?
- [ ] Before / after: drag the slider and use ← → on it (Tab to it first). Change the goal:
      the pictures update a moment later, "Working it out…" in between.
- [ ] "Play a 10 s sample": opens in your video player; does it look like the result?
- [ ] "Keep the first 1:40" on a video too long for Discord: Trim opens on that part, the hint
      gets better, Compress says "Compress 1:40".
- [ ] Trim: drag both grips, then ← → and Shift+← → on each (Tab reaches them); chapter chips;
      "Whole video"; closing Trim (the scissors) compresses the whole video again.
- [ ] Several files: drag three videos and a folder from Explorer onto the Compress page at
      once (all listed); remove one with ×; add more after a list finished (the finished rows go).
- [ ] A .ts recording with teletext or closed captions: they're left out (only text captions
      go into the .mp4) and the encode works.
- [ ] A file that fails (rename a .txt to .mp4 and add it to a list): it says why, the rest go on.
- [ ] Advanced: Smaller / Smallest with For Discord shows the warning; the graphics card
      tick says what this machine has (try on a PC without one); Sound "No sound"; untick
      "Keep all … sound tracks" on a film with two languages: only the first one in the result.
- [ ] Drop a video on the Download page: the "Clip it / Compress it" choice still works.
- [ ] Light theme: the goal rows, the slider tags, the trim strip.
- [ ] Screen reader: the goal rows read as radio buttons with their hint and size.
