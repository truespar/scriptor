<#
.SYNOPSIS
  Batch VISUAL-fidelity triage: render a folder of .docx with Scriptor AND LibreOffice, rasterize
  both to PNG, pixel-compare per page, and rank every doc by mean per-page difference. The pixel
  companion to `corpus-scorecard.ps1` - it catches what the GEOMETRY oracle is blind to (intra-line
  position, glyph rendering, borders / shading / table painting), because the scorecard only sees
  each paragraph's anchor (x, y), not the pixels.

.DESCRIPTION
  Per doc:
    Scriptor  -> `scriptor render`        -> page-NNN.png   (the engine, final view)
    Reference -> soffice --convert-to pdf -> ref-N.png      (LibreOffice, batch headless)
    Compare   -> magick compare -metric AE -> diff-NNN.png  + a per-page differing-pixel count

  Output: a ranked CSV (doc, status, sPages, rPages, meanPct, maxPct) sorted worst-first - hard
  failures (scriptor-crash / missing / ref-fail) on top, then page-count mismatches, then clean
  comparisons by mean difference - plus the diff images per doc under -WorkRoot so you can EYEBALL
  each hit.

  WHY LibreOffice, and the CAVEAT. soffice is batch-robust (no COM, no modal dialogs that hang an
  unattended run the way Word automation does), so it scales to a whole corpus. But LibreOffice is
  NOT Word: it lays tables / wraps / sizes differently, so a high meanPct can be Scriptor-vs-Word
  reflow that LO happens to disagree with, NOT a Scriptor bug. Treat this as a TRIAGE FILTER, not a
  score: rank candidates here, then EYEBALL the diff image (it localizes the divergence), and CONFIRM
  any real-looking hit against Word per-doc with `visual-diff.ps1 -Reference word <doc>`. A pure
  font-substitution floor (~1-2% from Carlito-vs-Calibri glyph edges) sits under everything; signal
  is what rises clearly above it. (This caveat is load-bearing: it's how the table-style-border and
  highlight-extent fixes were told apart from LO noise.)

.PARAMETER Dir       Folder of .docx to triage.
.PARAMETER List      Optional file of doc file-names (one per line) to restrict to. Empty = every
                     .docx in Dir (capped by -Max). Use it to re-run just a sample.
.PARAMETER Max       Cap the number of docs when no -List is given (0 = no cap).
.PARAMETER Out       Ranked CSV output path.
.PARAMETER WorkRoot  Where per-doc renders + diff images go.
.PARAMETER PdfDir    Where the LibreOffice PDFs are cached. An existing PDF is reused, so a re-run
                     after a Scriptor rebuild only re-renders Scriptor + re-compares (LO is the slow
                     part).
.PARAMETER Fuzz      Per-pixel colour tolerance for `magick compare` (default 15%) to discount
                     anti-aliasing / font-edge noise.
.PARAMETER Density   DPI for both renders (default 96; Scriptor scale = Density/96).

.NOTES
  Needs ImageMagick (`magick`) and LibreOffice (`soffice`) on PATH, plus a release build of the CLI
  (`cargo build -p scriptor-cli --release`). On Windows, if linking fails with LNK1104, dot-source
  scripts/dev-shell.ps1 first to load a complete MSVC environment.

.EXAMPLE
  # triage a sampled list of docs into a ranked CSV
  pwsh scripts/corpus-visual-diff.ps1 -Dir <corpus>/sw/qa/extras/ooxmlexport/data -List sample.txt -Out vdiff.csv
.EXAMPLE
  # then confirm a specific hit against WORD (the real target)
  pwsh scripts/visual-diff.ps1 -Reference word -Docx <corpus>/tdf117297_tableStyle.docx
#>
param(
  [Parameter(Mandatory = $true)][string]$Dir,
  [string]$List     = "",
  [int]$Max         = 0,
  [string]$Out      = "$env:TEMP\scriptor-vdiff\corpus.csv",
  [string]$WorkRoot = "$env:TEMP\scriptor-vdiff\work",
  [string]$PdfDir   = "$env:TEMP\scriptor-vdiff\pdf",
  [string]$Fuzz     = "15%",
  [int]$Density     = 96
)
# LibreOffice + magick write benign noise to stderr ("Document is empty", "platform independent
# libraries"); "Stop" would wrap that as a fatal NativeCommandError. Continue keeps the batch going.
$ErrorActionPreference = "Continue"
$scriptor = Join-Path $PSScriptRoot ("../target/release/scriptor" + $(if ($IsWindows -eq $false) { "" } else { ".exe" }))
$soffice  = (Get-Command soffice -ErrorAction SilentlyContinue).Source
if (-not $soffice) { $soffice = "C:\Program Files\LibreOffice\program\soffice.exe" }
if (-not (Test-Path $scriptor)) { throw "build the CLI first: cargo build -p scriptor-cli --release" }
if (-not (Get-Command magick -ErrorAction SilentlyContinue)) { throw "ImageMagick (magick) not on PATH" }
if (-not (Test-Path $soffice)) { throw "LibreOffice not found at $soffice" }
foreach ($d in @($WorkRoot, $PdfDir, (Split-Path $Out))) { if ($d) { New-Item -ItemType Directory -Force -Path $d | Out-Null } }

# The doc set: an explicit -List, else every .docx in -Dir (capped by -Max).
if ($List -and (Test-Path $List)) {
  $names = Get-Content $List | Where-Object { $_.Trim() -ne "" }
} else {
  $names = Get-ChildItem $Dir -Filter *.docx | Sort-Object Name | Select-Object -ExpandProperty Name
  if ($Max -gt 0) { $names = $names | Select-Object -First $Max }
}
$paths = @()
foreach ($n in $names) { $p = Join-Path $Dir $n; if (Test-Path $p) { $paths += $p } }

# Pass 1: LibreOffice -> PDF, reusing any PDF already cached, in chunks so one bad doc can't stall all.
$prof = "file:///" + ($WorkRoot -replace '\\', '/') + "/lo-profile"
$need = @($paths | Where-Object { -not (Test-Path (Join-Path $PdfDir ([IO.Path]::GetFileNameWithoutExtension($_) + ".pdf"))) })
Write-Host "== LibreOffice -> PDF ($($need.Count) of $($paths.Count) need converting) ==" -ForegroundColor Cyan
$chunk = 20
for ($i = 0; $i -lt $need.Count; $i += $chunk) {
  $slice = $need[$i..([math]::Min($i + $chunk - 1, $need.Count - 1))]
  & $soffice --headless --norestore "-env:UserInstallation=$prof" --convert-to pdf --outdir $PdfDir $slice 2>$null | Out-Null
  Write-Host ("  converted up to {0}/{1}" -f [math]::Min($i + $chunk, $need.Count), $need.Count)
}

# Pass 2: per doc - Scriptor render, then magick pdf->png + compare per page.
$rows = @()
$idx = 0
foreach ($n in $names) {
  $idx++
  $docx = Join-Path $Dir $n
  $base = [IO.Path]::GetFileNameWithoutExtension($n)
  $pdf  = Join-Path $PdfDir "$base.pdf"
  Write-Host ("[{0}/{1}] {2}" -f $idx, $names.Count, $n)
  if (-not (Test-Path $docx)) { $rows += [pscustomobject]@{doc=$n;status="missing";sPages=0;rPages=0;meanPct=0;maxPct=0}; continue }
  if (-not (Test-Path $pdf))  { $rows += [pscustomobject]@{doc=$n;status="ref-fail";sPages=0;rPages=0;meanPct=0;maxPct=0}; continue }

  $wd = Join-Path $WorkRoot $base
  $sdir = Join-Path $wd "s"; $rdir = Join-Path $wd "r"; $ddir = Join-Path $wd "d"
  foreach ($d in @($sdir, $rdir, $ddir)) { New-Item -ItemType Directory -Force -Path $d | Out-Null }

  # A native exe's non-zero exit does NOT throw under PS 5.1 ("Continue" preference), so check
  # $LASTEXITCODE explicitly - a crashed import used to fall through with 0 pages and meanPct 0,
  # ranking the doc as the BEST in the corpus instead of the worst.
  $renderOk = $true
  try {
    & $scriptor render $docx $sdir --scale ([math]::Round($Density / 96.0, 4)) --track none 2>$null | Out-Null
    if ($LASTEXITCODE -ne 0) { $renderOk = $false }
  }
  catch { $renderOk = $false }
  if (-not $renderOk) { $rows += [pscustomobject]@{doc=$n;status="scriptor-crash";sPages=0;rPages=0;meanPct=0;maxPct=0}; continue }

  & magick -density $Density -background white "$pdf" -alpha remove -alpha off "$rdir\ref-%d.png" 2>$null | Out-Null

  $spages = @(Get-ChildItem $sdir -Filter "page-*.png" | Sort-Object Name)
  $rpages = @(Get-ChildItem $rdir -Filter "ref-*.png" | Sort-Object { [int]($_.BaseName -replace 'ref-', '') })
  $pairs = [math]::Min($spages.Count, $rpages.Count)
  $sum = 0.0; $mx = 0.0
  for ($j = 0; $j -lt $pairs; $j++) {
    $s = $spages[$j].FullName
    $dim = (& magick identify -format "%wx%h" $s).Trim()
    $rfix = Join-Path $rdir ("fix-{0:d3}.png" -f ($j + 1))
    & magick "$($rpages[$j].FullName)" -background white -alpha remove -alpha off -resize "$dim!" "$rfix" 2>$null | Out-Null
    $diff = Join-Path $ddir ("diff-{0:d3}.png" -f ($j + 1))
    $wh = $dim -split 'x'; $px = [double]$wh[0] * [double]$wh[1]
    # `compare` writes the AE (# differing pixels) to stderr; capture it via cmd. Large counts print
    # in scientific notation ("2.25e+06"), so parse a full float token - the old leading-integer
    # parse read that as 2 and scored the worst pages ~0%.
    $ae = (cmd /c "magick compare -metric AE -fuzz $Fuzz `"$s`" `"$rfix`" `"$diff`" 2>&1") | Out-String
    $cmp = $LASTEXITCODE
    $aeNum = if ($ae -match '(\d[\d,]*\.?\d*(?:[eE][+-]?\d+)?)') { [double](($matches[1]) -replace ',', '') } else { 0.0 }
    if ($cmp -ge 2) { $aeNum = $px }  # exit >= 2 = comparison ERROR (exit 1 just means "images differ")
    $pct = if ($px -gt 0) { [math]::Round([math]::Min(100.0, 100.0 * $aeNum / $px), 2) } else { 0 }
    $sum += $pct; if ($pct -gt $mx) { $mx = $pct }
  }
  $mean = if ($pairs -gt 0) { [math]::Round($sum / $pairs, 2) } else { 0 }
  $st = if ($spages.Count -ne $rpages.Count) { "pagecount" } else { "ok" }
  $rows += [pscustomobject]@{doc=$n;status=$st;sPages=$spages.Count;rPages=$rpages.Count;meanPct=$mean;maxPct=$mx}
}
# Hard failures first (a crash / missing reference is the worst outcome, not a 0% match), then
# page-count mismatches, then clean comparisons - each bucket worst-first by mean difference.
$sevOrder = @{ "scriptor-crash" = 0; "missing" = 1; "ref-fail" = 2; "pagecount" = 3; "ok" = 4 }
$rows |
  Sort-Object -Property @{Expression = { $sevOrder[$_.status] }}, @{Expression = { $_.meanPct }; Descending = $true} |
  Export-Csv -NoTypeInformation -Encoding UTF8 $Out
Write-Host "wrote $Out ($($rows.Count) rows). Diff images per doc under $WorkRoot\<name>\d - EYEBALL the top rows." -ForegroundColor Green
