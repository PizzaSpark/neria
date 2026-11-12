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
  [int]$Wait = 5
)
$ErrorActionPreference = "Stop"
# "-Only a,b" through powershell -File arrives as one string.
$Sizes = @($Sizes | ForEach-Object { $_ -split "," } | Where-Object { $_ })
$Only = @($Only | ForEach-Object { $_ -split "," } | Where-Object { $_ })
$root = Split-Path $PSScriptRoot -Parent
$exe = Join-Path $root "target\debug\neria.exe"
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
# A signed-in site, for Settings → Network.
New-Item -ItemType Directory -Force (Join-Path $home_ "cookies") | Out-Null
"# Netscape HTTP Cookie File`n.youtube.com`tTRUE`t/`tTRUE`t0`tPREF`tx" | Set-Content -Encoding ascii (Join-Path $home_ "cookies\youtube.com.txt")

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
  "set-dl"     = @{ NERIA_SETTINGS = "download" }
  "set-about"  = @{ NERIA_SETTINGS = "about" }
  "set-net"    = @{ NERIA_SETTINGS = "network" }
  "signin"     = @{ NERIA_DIALOG = "signin" }
  "match"      = @{ NERIA_DIALOG = "match" }
  "sync"       = @{ NERIA_DIALOG = "sync" }
  "spotify"    = @{ NERIA_DIALOG = "spotify" }
  "update"     = @{ NERIA_DIALOG = "update" }
  "toast"      = @{ NERIA_DIALOG = "toast"; NERIA_DEMO = "1" }
  "drop"       = @{ NERIA_DROP_HOVER = $video }
  "drop-link"  = @{ NERIA_DROP_HOVER = "https://www.youtube.com/watch?v=jNQXAC9IVRw" }
  "drop-ask"   = @{ NERIA_DROP = $video }
}

Add-Type -AssemblyName System.Drawing
if (-not ("NeriaShot" -as [type])) {
Add-Type @"
using System; using System.Runtime.InteropServices;
public class NeriaShot {
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
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
[NeriaShot]::SetProcessDPIAware() | Out-Null

function Shot([string]$name, [string]$size, [hashtable]$envs) {
  $psi = New-Object System.Diagnostics.ProcessStartInfo $exe
  $psi.UseShellExecute = $false
  $psi.EnvironmentVariables["NERIA_HOME"] = $home_
  $psi.EnvironmentVariables["NERIA_WINDOW"] = $size
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
  $ims = $files | ForEach-Object { [System.Drawing.Image]::FromFile($_) }
  $tw = [math]::Min(560, ($ims | Measure-Object -Property Width -Maximum).Maximum)
  $cols = if ($tw -le 420) { 6 } else { 3 }
  $tiles = $ims | ForEach-Object { @{ im = $_; h = [int]($_.Height * $tw / $_.Width) } }
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
