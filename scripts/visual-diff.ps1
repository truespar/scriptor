<#
.SYNOPSIS
  Visual-fidelity diff for Scriptor: render a .docx with our engine AND a reference renderer
  (LibreOffice headless or Microsoft Word), rasterize both to PNG, and pixel-compare per page.

.DESCRIPTION
  This is layer 2 of the OOXML test bench (layer 1 = `scriptor coverage`). Coverage tells us which
  features EXIST in a doc; this tells us whether we render them CORRECTLY, and catches regressions
  (e.g. the footer / line-height churn) automatically.

  Pipeline per page:
    Scriptor  ->  scriptor render          -> page-NNN.png   (engine output)
    Reference ->  soffice/Word -> PDF      -> page-NNN.png   (ground truth, rasterized at -density)
    Compare   ->  magick compare -metric AE                  -> diff-NNN.png + a difference score

  Output: a per-page table (differing-pixel count + % of page) and a diff image per page in WorkDir,
  so you can eyeball exactly where we diverge.

.PARAMETER Docx       Path to the .docx to test.
.PARAMETER Reference  "soffice" (default, EU-friendly) or "word" (truest Word fidelity, needs Word).
.PARAMETER Density    DPI for both renders (default 96; Scriptor scale = Density/96).
.PARAMETER Fuzz       Per-pixel color tolerance for `compare` (default 10%) to ignore anti-aliasing.
.PARAMETER Track      Tracked-change display to compare in: "all" (markup shown), "none"/"final"
                      (deletions hidden), or "original" (insertions hidden). Scriptor renders in this
                      mode; the Word reference is made to match (accept-all for none, reject-all for
                      original) so a Final-view pagination comparison lines up. Default "all".
.PARAMETER WorkDir    Where to put renders + diffs (default %TEMP%\scriptor-vdiff\<name>).

.EXAMPLE
  pwsh scripts/visual-diff.ps1 -Docx ./sample.docx
  pwsh scripts/visual-diff.ps1 -Docx ./sample.docx -Reference word
  pwsh scripts/visual-diff.ps1 -Docx ./redline.docx -Reference word -Track none
#>
param(
  [Parameter(Mandatory = $true)][string]$Docx,
  [ValidateSet("soffice", "word")][string]$Reference = "soffice",
  [int]$Density = 96,
  [string]$Fuzz = "10%",
  [ValidateSet("all", "simple", "none", "final", "original")][string]$Track = "all",
  [string]$WorkDir = ""
)
$ErrorActionPreference = "Stop"

$scriptor = Join-Path $PSScriptRoot ("../target/release/scriptor" + $(if ($IsWindows -eq $false) { "" } else { ".exe" }))
# Prefer soffice on PATH (how it is installed nearly everywhere but a default Windows install).
$soffice = (Get-Command soffice -ErrorAction SilentlyContinue).Source
if (-not $soffice) { $soffice = "C:\Program Files\LibreOffice\program\soffice.exe" }
if (-not (Test-Path $scriptor)) { throw "build the CLI first: cargo build -p scriptor-cli --release" }
if (-not (Get-Command magick -ErrorAction SilentlyContinue)) { throw "ImageMagick (magick) not on PATH" }

$Docx = (Resolve-Path $Docx).Path
$name = [IO.Path]::GetFileNameWithoutExtension($Docx)
if (-not $WorkDir) { $WorkDir = Join-Path $env:TEMP "scriptor-vdiff\$name" }
$sdir = Join-Path $WorkDir "scriptor"
$rdir = Join-Path $WorkDir "ref"
$ddir = Join-Path $WorkDir "diff"
foreach ($d in @($sdir, $rdir, $ddir)) { New-Item -ItemType Directory -Force -Path $d | Out-Null }

Write-Host "== Scriptor render (track=$Track) ==" -ForegroundColor Cyan
& $scriptor render $Docx $sdir --scale ([math]::Round($Density / 96.0, 4)) --track $Track | Out-Null

Write-Host "== Reference render ($Reference) ==" -ForegroundColor Cyan
$pdf = Join-Path $WorkDir "$name.pdf"
if ($Reference -eq "soffice") {
  if (-not (Test-Path $soffice)) { throw "LibreOffice not found at $soffice" }
  $profile = "file:///" + ($WorkDir -replace '\\', '/') + "/lo-profile"
  & $soffice --headless --norestore "-env:UserInstallation=$profile" --convert-to pdf --outdir $WorkDir $Docx | Out-Null
}
else {
  # Word COM -> PDF (format 17 = wdExportFormatPDF). Suppress ALL dialogs so a tracked-changes /
  # conversion / repair prompt can't hang the automation. Only ever touch the instance we launch.
  $word = New-Object -ComObject Word.Application
  $word.Visible = $false
  $word.DisplayAlerts = 0       # wdAlertsNone
  $word.Options.ConfirmConversions = $false
  $word.AutomationSecurity = 3  # msoAutomationSecurityForceDisable (no macros)
  try {
    # Open(FileName, ConfirmConversions=$false, ReadOnly=$true, AddToRecentFiles=$false)
    $doc = $word.Documents.Open($Docx, $false, $true, $false)
    # Make the reference match Scriptor's track mode by resolving revisions in memory (discarded on
    # Close): accept-all removes deletions (the Final view); reject-all removes insertions (Original).
    # "all" leaves the markup in place. Edits to a read-only doc are in-memory only, so nothing saves.
    switch ($Track) {
      "none"     { try { $doc.AcceptAllRevisions() } catch {} }
      "final"    { try { $doc.AcceptAllRevisions() } catch {} }
      "simple"   { try { $doc.AcceptAllRevisions() } catch {} }
      "original" { try { $doc.RejectAllRevisions() } catch {} }
    }
    $doc.ExportAsFixedFormat($pdf, 17)
    $doc.Close($false)          # wdDoNotSaveChanges
  }
  finally {
    try { $word.Quit($false) } catch {}
    [System.Runtime.InteropServices.Marshal]::ReleaseComObject($word) | Out-Null
  }
}
if (-not (Test-Path $pdf)) { throw "reference PDF was not produced: $pdf" }

# PDF -> per-page PNG (ImageMagick names them 0-based: ref-0.png, ref-1.png, ...). Flatten onto
# white + drop alpha so the transparent PDF background matches Scriptor's opaque white page.
& magick -density $Density -background white "$pdf" -alpha remove -alpha off "$rdir\ref-%d.png" | Out-Null

$spages = @(Get-ChildItem $sdir -Filter "page-*.png" | Sort-Object Name)
$rpages = @(Get-ChildItem $rdir -Filter "ref-*.png" | Sort-Object { [int]($_.BaseName -replace 'ref-', '') })

Write-Host ""
Write-Host ("Scriptor: {0} page(s)   Reference: {1} page(s)" -f $spages.Count, $rpages.Count) -ForegroundColor Yellow
if ($spages.Count -ne $rpages.Count) {
  Write-Host "  ! page-count mismatch (a pagination fidelity gap in itself)" -ForegroundColor Red
}

$pairs = [math]::Min($spages.Count, $rpages.Count)
Write-Host ""
Write-Host ("{0,5}  {1,12}  {2,8}  diff image" -f "page", "diff-pixels", "% page")
$totalPct = 0.0
for ($i = 0; $i -lt $pairs; $i++) {
  $s = $spages[$i].FullName
  $r = $rpages[$i].FullName
  $dim = (& magick identify -format "%wx%h" $s).Trim()
  $rfix = Join-Path $rdir ("fix-{0:d3}.png" -f ($i + 1))
  & magick "$r" -background white -alpha remove -alpha off -resize "$dim!" "$rfix" | Out-Null
  $diff = Join-Path $ddir ("diff-{0:d3}.png" -f ($i + 1))
  $wh = $dim -split 'x'; $px = [double]$wh[0] * [double]$wh[1]
  # compare writes the AE (absolute error = # differing pixels) to stderr; capture via cmd.
  $ae = (cmd /c "magick compare -metric AE -fuzz $Fuzz `"$s`" `"$rfix`" `"$diff`" 2>&1") | Out-String
  $cmp = $LASTEXITCODE
  # compare prints the count in scientific notation once it exceeds ~1e6 (e.g. "2.25e+06 (1)"), so
  # parse a full float token - grabbing only the leading integer digits scored the WORST pages ~0%.
  $aeNum = if ($ae -match '(\d[\d,]*\.?\d*(?:[eE][+-]?\d+)?)') { [double](($matches[1]) -replace ',', '') } else { 0.0 }
  if ($cmp -ge 2) { $aeNum = $px }  # exit >= 2 = comparison ERROR (exit 1 just means "images differ")
  $pct = if ($px -gt 0) { [math]::Round([math]::Min(100.0, 100.0 * $aeNum / $px), 2) } else { 0 }
  $totalPct += $pct
  Write-Host ("{0,5}  {1,12:n0}  {2,7}%  {3}" -f ($i + 1), $aeNum, $pct, $diff)
}
if ($pairs -gt 0) {
  Write-Host ""
  Write-Host ("mean per-page difference: {0}%" -f [math]::Round($totalPct / $pairs, 2)) -ForegroundColor Green
  Write-Host "diff images (red = mismatch) in: $ddir"
}
