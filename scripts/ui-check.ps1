<#
.SYNOPSIS
  Screenshot every page and dialog of Neria at several window widths, one contact sheet
  per width, to check a UI change didn't clip or overlap anything (docs/DESIGN.md).

.DESCRIPTION
  Needs a debug build whose tools exist (`cargo run -p neria` once). Uses its own scratch
  home in target/ui-check (bin is a junction to target/debug/bin, sample media is made
  with ffmpeg), so nothing touches your settings, queue or Videos folder. Windows are
  captured even when covered (PrintWindow), but not while the screen is off or asleep:
  the script says so when every shot comes out blank.

.EXAMPLE
  powershell -File scripts/ui-check.ps1
  powershell -File scripts/ui-check.ps1 -Sizes 400x600 -Only dl,clip,match
#>
param(
  [string[]]$Sizes = @("1280x800", "900x640", "720x600", "400x600"),
  [string[]]$Only = @(),
  [int]$Wait = 5,
  # Another build (e.g. target\clip\debug\neria.exe while the usual one is running).
  [string]$Exe = "",
  # Open the windows on the main screen too (by default they go to another screen, if
  # there is one, and never take the keyboard: you can keep working or playing).
  [switch]$MainScreen
)
$ErrorActionPreference = "Stop"
# "-Only a,b" through powershell -File arrives as one string.
$Sizes = @($Sizes | ForEach-Object { $_ -split "," } | Where-Object { $_ })
$Only = @($Only | ForEach-Object { $_ -split "," } | Where-Object { $_ })
$root = Split-Path $PSScriptRoot -Parent
$exe = if ($Exe) { (Resolve-Path $Exe).Path } else { Join-Path $root "target\debug\neria.exe" }
$bin = Join-Path $root "target\debug\bin"
$out = Join-Path $root "target\ui-check"
$home_ = Join-Path $out "home"
$media = Join-Path $out "media"
$shots = Join-Path $out "shots"
if (-not (Test-Path $exe)) { throw "Build first: cargo build -p neria" }
if (-not (Test-Path (Join-Path $bin "ffmpeg.exe"))) { throw "No tools in target\debug\bin: run 'cargo run -p neria' once" }
New-Item -ItemType Directory -Force $home_, $media, $shots | Out-Null
if (-not (Test-Path (Join-Path $home_ "bin"))) {
  cmd /c mklink /J "$(Join-Path $home_ 'bin')" "$bin" | Out-Null
}

# Downloads from scripted checks land in target/ui-check/downloads, never your own folder.
$settingsFile = Join-Path $home_ "settings.json"
$dl = Join-Path $out "downloads"
$cfg = if (Test-Path $settingsFile) { Get-Content -Raw $settingsFile | ConvertFrom-Json } else { [pscustomobject]@{} }
$cfg | Add-Member -NotePropertyName output_dir -NotePropertyValue $dl -Force
[IO.File]::WriteAllText($settingsFile, ($cfg | ConvertTo-Json -Depth 10))

# Sample media and a download history (for the Clip / Compress "Recent downloads").
$ff = Join-Path $bin "ffmpeg.exe"
$samples = @(
  @{ f = "sample.mp4"; t = "Lo-fi beats to study to, 3 hour mix"; args = @("-f","lavfi","-i","testsrc2=size=1280x720:rate=30","-f","lavfi","-i","sine=frequency=440","-t","20","-c:v","libx264","-pix_fmt","yuv420p","-c:a","aac","-shortest") },
  @{ f = "timelapse.mp4"; t = "Mountain timelapse 8K"; args = @("-f","lavfi","-i","mandelbrot=size=640x360:rate=25","-t","8","-c:v","libx264","-pix_fmt","yuv420p") },
  @{ f = "song.mp3"; t = "a-ha - Take On Me"; args = @("-f","lavfi","-i","sine=frequency=330","-t","30") }
)
foreach ($s in $samples) {
  $p = Join-Path $media $s.f
  if (-not (Test-Path $p)) { & $ff -loglevel error -y @($s.args) $p }
}
$now = [int][double]::Parse((Get-Date -UFormat %s))
$history = @($samples | ForEach-Object -Begin { $i = 0 } -Process {
  $i++; @{ key = "Sample:$i"; url = "https://example.com/$i"; file = (Join-Path $media $_.f); title = $_.t; at = $now - $i * 7200 }
})
$history | ConvertTo-Json | Set-Content -Encoding ascii (Join-Path $home_ "history.json")
$video = Join-Path $media "sample.mp4"
# The same video with chapter marks, for the clipper's chapter strip.
$chaptered = Join-Path $media "chapters.mp4"
if (-not (Test-Path $chaptered)) {
  $meta = Join-Path $media "chapters.txt"
  $lines = @(";FFMETADATA1")
  $names = @("Intro", "The twilight zone", "Midnight zone", "Credits")
  for ($c = 0; $c -lt 4; $c++) { $lines += "[CHAPTER]", "TIMEBASE=1/1000", "START=$($c * 5000)", "END=$(($c + 1) * 5000)", "title=$($names[$c])" }
  $lines | Set-Content -Encoding ascii $meta
  & $ff -loglevel error -y -i $video -i $meta -map 0 -map_chapters 1 -c copy $chaptered
}
# Ten minutes with a sound that rises and falls, for the zoomed timeline (Clip track 9).
$long = Join-Path $media "long.mp4"
if (-not (Test-Path $long)) {
  & $ff -loglevel error -y -f lavfi -i "testsrc2=size=640x360:rate=25" -f lavfi -i "aevalsrc=sin(440*2*PI*t)*abs(sin(0.7*t))*(0.3+0.7*abs(sin(0.05*t))):s=44100" -t 600 -c:v libx264 -preset ultrafast -pix_fmt yuv420p -g 50 -c:a aac -shortest $long
}
$song = Join-Path $media "song.mp3"
# Six minutes of stereo WAV (~63 MB): big enough for every Compress goal to make it smaller.
$talk = Join-Path $media "talk.wav"
if (-not (Test-Path $talk)) {
  & $ff -loglevel error -y -f lavfi -i "aevalsrc=sin(220*2*PI*t)*abs(sin(0.9*t))|sin(330*2*PI*t)*abs(sin(0.6*t)):s=44100" -t 360 -c:a pcm_s16le $talk
}
# A letterboxed film with two sound tracks, captions and chapters (Compress: black bars, tracks).
$film = Join-Path $media "film.mkv"
if (-not (Test-Path $film)) {
  $srt = Join-Path $media "film.srt"
  "1`n00:00:01,000 --> 00:00:04,000`nHello there`n" | Set-Content -Encoding ascii $srt
  $fmeta = Join-Path $media "film.txt"
  @(";FFMETADATA1", "[CHAPTER]", "TIMEBASE=1/1", "START=0", "END=12", "title=Opening", "[CHAPTER]", "TIMEBASE=1/1", "START=12", "END=30", "title=The chase", "[CHAPTER]", "TIMEBASE=1/1", "START=30", "END=40", "title=Credits") | Set-Content -Encoding ascii $fmeta
  & $ff -loglevel error -y -f lavfi -i "testsrc2=s=1920x800:r=30:d=40,pad=1920:1080:0:140" -f lavfi -i "sine=f=440:d=40" -f lavfi -i "sine=f=660:d=40" -i $srt -i $fmeta -map 0 -map 1 -map 2 -map 3 -map_chapters 4 -c:v libx264 -preset ultrafast -b:v 6M -c:a aac -c:s srt $film
}
# A .gif, for opening one in the clipper.
$gifFile = Join-Path $media "loop.gif"
if (-not (Test-Path $gifFile)) { & $ff -loglevel error -y -i $video -t 4 -vf "fps=12,scale=360:-2" $gifFile }
# A signed-in site, for Settings → Network.
New-Item -ItemType Directory -Force (Join-Path $home_ "cookies") | Out-Null
"# Netscape HTTP Cookie File`n.youtube.com`tTRUE`t/`tTRUE`t0`tPREF`tx" | Set-Content -Encoding ascii (Join-Path $home_ "cookies\youtube.com.txt")

# A followed channel and playlist, for Settings → Following.
@(
  @{ url = "https://www.youtube.com/@kurzgesagt/videos"; name = "Kurzgesagt - In a Nutshell"; channel = $true; every = "daily"; last_check = ($now - 7200); last_new = 1 },
  @{ url = "https://www.youtube.com/playlist?list=PL1"; name = "Lo-fi beats to study to"; every = "weekly"; paused = $true; last_check = ($now - 90000) }
) | ConvertTo-Json | ForEach-Object { [IO.File]::WriteAllText((Join-Path $home_ "following.json"), $_) }

# What to capture: name -> dev hooks (see CLAUDE.md).
$states = [ordered]@{
  "dl"         = @{ NERIA_DEMO = "1" }
  "dl-look"    = @{ NERIA_DEMO = "1"; NERIA_FILTER = "attention" }
  "dl-empty"   = @{}
  "pick"       = @{ NERIA_DIALOG = "pick" }
  "clip-empty" = @{ NERIA_PAGE = "clip" }
  "clip"       = @{ NERIA_OPEN = $video }
  "cmp-empty"  = @{ NERIA_PAGE = "compress" }
  "cmp"        = @{ NERIA_COMPRESS = $video }
  # Compress track: a film (bars, tracks) with Advanced open; too long for Discord; trimmed
  # (with chapters); a "keeping" goal; several files.
  "cmp-film"   = @{ NERIA_COMPRESS = $film; NERIA_COMPRESS_ADVANCED = "1" }
  "cmp-long"   = @{ NERIA_COMPRESS = $long; NERIA_COMPRESS_GOAL = "whatsapp" }
  "cmp-trim"   = @{ NERIA_COMPRESS = $film; NERIA_COMPRESS_TRIM = "12,30" }
  "cmp-sharp"  = @{ NERIA_COMPRESS = $long; NERIA_COMPRESS_GOAL = "sharp" }
  "cmp-many"   = @{ NERIA_COMPRESS = "$video;$long;$film;$(Join-Path $media 'timelapse.mp4')" }
  # A sound file: goals in sound words, no pictures, Advanced is just the format.
  "cmp-song"   = @{ NERIA_COMPRESS = $song; NERIA_COMPRESS_ADVANCED = "1" }
  "cmp-talk"   = @{ NERIA_COMPRESS = $talk; NERIA_COMPRESS_ADVANCED = "1" }
  "cmp-talk-tiny" = @{ NERIA_COMPRESS = $talk; NERIA_COMPRESS_GOAL = "2" }
  # Parts kept (joined / one file each), Volume evened out, the sample playing in the page.
  "cmp-parts"  = @{ NERIA_COMPRESS = $talk; NERIA_COMPRESS_PARTS = "20-80,150-200,300-330"; NERIA_COMPRESS_EVEN = "14"; NERIA_COMPRESS_ADVANCED = "1" }
  "cmp-parts-each" = @{ NERIA_COMPRESS = $film; NERIA_COMPRESS_PARTS = "2-8,20-26"; NERIA_COMPRESS_EACH = "1" }
  "cmp-play"   = @{ NERIA_COMPRESS = $film; NERIA_COMPRESS_PLAY = "1" }
  "cmp-play-song" = @{ NERIA_COMPRESS = $talk; NERIA_COMPRESS_PLAY = "1" }
  "cmp-song-mix" = @{ NERIA_COMPRESS = "$video;$song" }
  "set-dl"     = @{ NERIA_SETTINGS = "download" }
  "set-about"  = @{ NERIA_SETTINGS = "about" }
  "set-net"    = @{ NERIA_SETTINGS = "network" }
  "set-follow" = @{ NERIA_SETTINGS = "following" }
  "set-spotify" = @{ NERIA_SETTINGS = "spotify" }
  "set-lyrics" = @{ NERIA_SETTINGS = "spotify"; NERIA_LYRICS_SPOTIFY = "1" }
  "set-lyrics-in" = @{ NERIA_SETTINGS = "spotify"; NERIA_LYRICS_SPOTIFY = "signed-in" }
  "dl-spotify" = @{ NERIA_URL = "https://open.spotify.com/album/4m2880jivSbbyEGAKfITCa" }
  "signin"     = @{ NERIA_DIALOG = "signin" }
  "extras"     = @{ NERIA_DIALOG = "extras" }
  "advanced"   = @{ NERIA_DIALOG = "advanced" }
  "captions"   = @{ NERIA_DIALOG = "captions" }
  "audio"      = @{ NERIA_DIALOG = "audio" }
  "chapters"   = @{ NERIA_DIALOG = "chapters" }
  "set-general" = @{ NERIA_SETTINGS = "general" }
  "set-sound"  = @{ NERIA_SETTINGS = "general"; NERIA_OWN_SOUND = "demo" }
  "set-sound-gone" = @{ NERIA_SETTINGS = "general"; NERIA_OWN_SOUND = "missing" }
  "close-ask"  = @{ NERIA_DIALOG = "close"; NERIA_DEMO = "1" }
  "close-rec"  = @{ NERIA_DIALOG = "close-rec"; NERIA_DEMO = "1" }
  # Live streams: the preview (from the start / not on yet / on for days) over the demo
  # queue, whose first rows are recordings.
  "live"       = @{ NERIA_DIALOG = "live"; NERIA_DEMO = "1" }
  "live-soon"  = @{ NERIA_DIALOG = "live-upcoming"; NERIA_DEMO = "1" }
  "live-long"  = @{ NERIA_DIALOG = "live-long"; NERIA_DEMO = "1" }
  "later"      = @{ NERIA_DIALOG = "later" }
  "names"      = @{ NERIA_DIALOG = "names" }
  "clip-ch"    = @{ NERIA_OPEN = $chaptered; NERIA_CLIP_IN = "5"; NERIA_CLIP_OUT = "10" }
  "sound"      = @{ NERIA_OPEN = $video; NERIA_DIALOG = "sound" }
  "sound-gain" = @{ NERIA_OPEN = $video; NERIA_DIALOG = "sound-gain" }
  "gif"        = @{ NERIA_OPEN = $video; NERIA_DIALOG = "gif"; NERIA_CLIP_IN = "3"; NERIA_CLIP_OUT = "9" }
  "clip-gif"   = @{ NERIA_OPEN = $gifFile }
  # The timeline zoomed in (40 s of 10 min), and a sound-only file zoomed.
  "clip-zoom"  = @{ NERIA_OPEN = $long; NERIA_CLIP_IN = "212"; NERIA_CLIP_OUT = "230"; NERIA_CLIP_VIEW = "200,40" }
  "clip-zoom-ch" = @{ NERIA_OPEN = $chaptered; NERIA_CLIP_IN = "5"; NERIA_CLIP_OUT = "10"; NERIA_CLIP_VIEW = "3,8" }
  "clip-zoom-song" = @{ NERIA_OPEN = $song; NERIA_CLIP_IN = "8"; NERIA_CLIP_OUT = "12"; NERIA_CLIP_VIEW = "5,10" }
  "picture"    = @{ NERIA_OPEN = $video; NERIA_DIALOG = "picture" }
  "picture-result" = @{ NERIA_OPEN = $video; NERIA_DIALOG = "picture-result" }
  "clips"      = @{ NERIA_OPEN = $chaptered; NERIA_DIALOG = "clips" }
  "clips-page" = @{ NERIA_OPEN = $chaptered; NERIA_DIALOG = "clips-page" }
  "sound-only" = @{ NERIA_OPEN = $video; NERIA_DIALOG = "sound-only"; NERIA_CLIP_IN = "3"; NERIA_CLIP_OUT = "9" }
  # A sound file: Save as (the same / another format), and the page saying what it'll do.
  "sound-song" = @{ NERIA_OPEN = $song; NERIA_DIALOG = "sound" }
  "sound-as"   = @{ NERIA_OPEN = $song; NERIA_DIALOG = "sound-as" }
  "clip-song"  = @{ NERIA_OPEN = $song; NERIA_CLIP_IN = "3"; NERIA_CLIP_OUT = "9" }
  # A link in the clipper: the quality row too (looked up for real, like "bulk").
  "clip-link"  = @{ NERIA_OPEN = "https://www.youtube.com/watch?v=jNQXAC9IVRw" }
  # The same link zoomed in: close-ups from its storyboard.
  "clip-link-zoom" = @{ NERIA_OPEN = "https://www.youtube.com/watch?v=jNQXAC9IVRw"; NERIA_CLIP_IN = "5"; NERIA_CLIP_OUT = "9"; NERIA_CLIP_VIEW = "4,6" }
  # A live stream in the clipper (a 24/7 channel; needs the internet and a long -Wait).
  "clip-live"  = @{ NERIA_OPEN = "https://www.youtube.com/@aljazeeraenglish/live" }
  "clip-live-buffer" = @{ NERIA_OPEN = "https://www.twitch.tv/lofigirl" }
  "watch-live" = @{ NERIA_WATCH = "https://www.youtube.com/@aljazeeraenglish/live" }
  "match"      = @{ NERIA_DIALOG = "match" }
  "sync"       = @{ NERIA_DIALOG = "sync" }
  "spotify"    = @{ NERIA_DIALOG = "spotify" }
  "update"     = @{ NERIA_DIALOG = "update" }
  "toast"      = @{ NERIA_DIALOG = "toast"; NERIA_DEMO = "1" }
  "drop"       = @{ NERIA_DROP_HOVER = $video }
  "drop-link"  = @{ NERIA_DROP_HOVER = "https://www.youtube.com/watch?v=jNQXAC9IVRw" }
  "drop-ask"   = @{ NERIA_DROP = $video }
  # Several links at once (looked up for real: needs the internet and a longer -Wait).
  "bulk"       = @{ NERIA_URL = "https://www.youtube.com/watch?v=jNQXAC9IVRw https://www.youtube.com/watch?v=jNQXAC9IVRw&list=PLBCF2DAC6FFB574DE https://vimeo.com/76979871 spotify:album:1 https://example.com/nothing-here" }
  "watch"      = @{ NERIA_WATCH = $video }
  # A single-video preview with its play mark (looked up for real, like "bulk").
  "dl-link"    = @{ NERIA_URL = "https://www.youtube.com/watch?v=jNQXAC9IVRw" }
  "copied"     = @{ NERIA_COPIED = "https://www.youtube.com/watch?v=jNQXAC9IVRw"; NERIA_DEMO = "1" }
}

Add-Type -AssemblyName System.Drawing
if (-not ("NeriaShot" -as [type])) {
Add-Type @"
using System; using System.Runtime.InteropServices;
public class NeriaShot {
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
  [DllImport("user32.dll")] public static extern bool SetProcessDpiAwarenessContext(IntPtr c);
  [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr dc, uint f);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool GetClientRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  public delegate bool EP(IntPtr h, IntPtr l);
  [DllImport("user32.dll")] public static extern bool EnumWindows(EP cb, IntPtr l);
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
  public static IntPtr Find(uint pid) {
    IntPtr best = IntPtr.Zero; long area = 0;
    EnumWindows((h, l) => { uint p; GetWindowThreadProcessId(h, out p);
      if (p == pid && IsWindowVisible(h)) { RECT r; GetWindowRect(h, out r); long a = (long)(r.R-r.L)*(r.B-r.T); if (a > area) { area = a; best = h; } }
      return true; }, IntPtr.Zero);
    return area > 40000 ? best : IntPtr.Zero;
  }
}
"@
}
# Per-monitor aware, so a window on a screen with another scale is measured in its own pixels.
if (-not [NeriaShot]::SetProcessDpiAwarenessContext([IntPtr]::new(-4))) { [NeriaShot]::SetProcessDPIAware() | Out-Null }
# Where the windows open (physical px, now that this script is DPI-aware).
Add-Type -AssemblyName System.Windows.Forms
$other = [System.Windows.Forms.Screen]::AllScreens | Where-Object { -not $_.Primary } | Select-Object -First 1
$windowPos = if ($other -and -not $MainScreen) { "$($other.Bounds.X + 20),$($other.Bounds.Y + 20)" } else { "" }

function Shot([string]$name, [string]$size, [hashtable]$envs) {
  $psi = New-Object System.Diagnostics.ProcessStartInfo $exe
  $psi.UseShellExecute = $false
  $psi.EnvironmentVariables["NERIA_HOME"] = $home_
  $psi.EnvironmentVariables["NERIA_WINDOW"] = $size
  if ($windowPos) { $psi.EnvironmentVariables["NERIA_WINDOW_POS"] = $windowPos }
  foreach ($k in $envs.Keys) { $psi.EnvironmentVariables[$k] = $envs[$k] }
  $p = [System.Diagnostics.Process]::Start($psi)
  try {
    $deadline = (Get-Date).AddSeconds(20)
    while ([NeriaShot]::Find([uint32]$p.Id) -eq 0 -and (Get-Date) -lt $deadline) { Start-Sleep -Milliseconds 200 }
    Start-Sleep -Seconds $Wait
    $h = [NeriaShot]::Find([uint32]$p.Id)
    $r = New-Object NeriaShot+RECT
    [NeriaShot]::GetWindowRect($h, [ref]$r) | Out-Null
    $w = $r.R - $r.L; $ht = $r.B - $r.T
    $bmp = New-Object System.Drawing.Bitmap $w, $ht
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $dc = $g.GetHdc(); [NeriaShot]::PrintWindow($h, $dc, 2) | Out-Null; $g.ReleaseHdc($dc); $g.Dispose()
    # Back to logical pixels (the window was asked for $size logical px).
    $c = New-Object NeriaShot+RECT; [NeriaShot]::GetClientRect($h, [ref]$c) | Out-Null
    $scale = [math]::Max(1.0, ($c.R - $c.L) / [double]$size.Split("x")[0])
    $small = New-Object System.Drawing.Bitmap ([int]($w / $scale)), ([int]($ht / $scale))
    $g2 = [System.Drawing.Graphics]::FromImage($small)
    $g2.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
    $g2.DrawImage($bmp, 0, 0, $small.Width, $small.Height); $g2.Dispose(); $bmp.Dispose()
    $file = Join-Path $shots "$name.png"
    $small.Save($file, [System.Drawing.Imaging.ImageFormat]::Png)
    $px = $small.GetPixel([int]($small.Width / 2), [int]($small.Height / 2)); $small.Dispose()
    return ($px.R -eq 255 -and $px.G -eq 255 -and $px.B -eq 255)
  } finally {
    if (-not $p.HasExited) { $p.Kill() }
  }
}

function Sheet([string]$size, [string[]]$files) {
  $ims = @($files | ForEach-Object { [System.Drawing.Image]::FromFile($_) })
  $tw = [math]::Min(560, ($ims | Measure-Object -Property Width -Maximum).Maximum)
  $cols = if ($tw -le 420) { 6 } else { 3 }
  $tiles = @($ims | ForEach-Object { @{ im = $_; h = [int]($_.Height * $tw / $_.Width) } })
  $th = ($tiles | ForEach-Object { $_.h } | Measure-Object -Maximum).Maximum + 22
  $rows = [math]::Ceiling($ims.Count / $cols)
  $sheet = New-Object System.Drawing.Bitmap ($cols * ($tw + 10) + 10), ($rows * ($th + 10) + 10)
  $g = [System.Drawing.Graphics]::FromImage($sheet)
  $g.Clear([System.Drawing.Color]::FromArgb(230, 225, 215))
  $g.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
  $font = New-Object System.Drawing.Font "Segoe UI", 9
  for ($i = 0; $i -lt $ims.Count; $i++) {
    $x = 10 + ($i % $cols) * ($tw + 10); $y = 10 + [math]::Floor($i / $cols) * ($th + 10)
    $g.DrawString([IO.Path]::GetFileNameWithoutExtension($files[$i]), $font, [System.Drawing.Brushes]::Black, $x, $y)
    $g.DrawImage($tiles[$i].im, $x, $y + 18, $tw, $tiles[$i].h)
  }
  $g.Dispose(); $ims | ForEach-Object { $_.Dispose() }
  $path = Join-Path $out "sheet-$size.png"
  $sheet.Save($path, [System.Drawing.Imaging.ImageFormat]::Png); $sheet.Dispose()
  return $path
}

$blank = 0; $total = 0
$baseWait = $Wait
foreach ($size in $Sizes) {
  $files = @()
  foreach ($name in $states.Keys) {
    if ($Only.Count -gt 0 -and -not ($Only -contains $name)) { continue }
    # Clip and Compress read the file first: give them a little longer.
    $script:Wait = $baseWait + $(if ($name -like "clip*" -or $name -like "cmp*") { 2 } else { 0 })
    if (Shot "$name-$size" $size $states[$name]) { $blank++ }
    $total++
    $files += Join-Path $shots "$name-$size.png"
  }
  $script:Wait = $baseWait
  if ($files.Count -gt 0) { Write-Host "$size -> $(Sheet $size $files)" }
}
if ($total -gt 0 -and $blank -eq $total) {
  Write-Warning "Every shot is blank white: the screen is probably off or asleep. Wake it and run again."
}
