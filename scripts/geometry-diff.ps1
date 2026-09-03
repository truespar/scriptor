<#
.SYNOPSIS
  The geometry oracle: diff Scriptor's per-paragraph layout against real Word, in points, and report
  exactly where (and by how much) they diverge.

.DESCRIPTION
  Runs `scriptor geometry` (the engine's own caret geometry) and `word-geometry.ps1` (Word via COM),
  both in points, then pairs paragraphs by their text and reports per-paragraph deltas:
    - page mismatch          (a pagination divergence)
    - dY  > tolerance         (vertical drift - the "this paragraph is N pt too high/low" signal)
    - dX  > tolerance         (indent / horizontal divergence)
    - list-marker mismatch    (Word numbered it "1.1" and we didn't, or differently)

  Unlike a pixel diff this is immune to font-rasterizer noise and names the paragraph. The summary's
  median dY surfaces a SYSTEMATIC offset (e.g. "every paragraph 26 pt high" = a spacing/metrics bug)
  vs one-off divergences.

  Paragraphs are matched by normalized text (whitespace-collapsed, lower-cased). Empty paragraphs
  (spacers) carry no text to match on, so the content comparison is over non-empty paragraphs;
  unmatched ones on each side are reported (a structure/pagination gap in itself).

.PARAMETER Docx   Path to the .docx.
.PARAMETER Tol    Divergence threshold in points (default 2 - about 1px at 96 DPI).
.PARAMETER Track  Tracked-change view both sides measure in (default "all").
.PARAMETER Top    How many worst-dY rows to show (default 20).

.EXAMPLE
  pwsh scripts/geometry-diff.ps1 -Docx contract.docx
#>
param(
  [Parameter(Mandatory = $true)][string]$Docx,
  [double]$Tol = 2.0,
  [string]$Track = "all",
  [int]$Top = 20
)
$ErrorActionPreference = "Stop"

$scriptor = Join-Path $PSScriptRoot ("../target/release/scriptor" + $(if ($IsWindows -eq $false) { "" } else { ".exe" }))
if (-not (Test-Path $scriptor)) { throw "build the CLI first: cargo build -p scriptor-cli --release" }
$Docx = (Resolve-Path $Docx).Path
$name = [IO.Path]::GetFileNameWithoutExtension($Docx)
$dir = Join-Path $env:TEMP "scriptor-geom"
New-Item -ItemType Directory -Force -Path $dir | Out-Null
$sPath = Join-Path $dir "$name.scriptor.json"
$wPath = Join-Path $dir "$name.word.json"

Write-Host "== Scriptor geometry ==" -ForegroundColor Cyan
& $scriptor geometry $Docx --out $sPath --track $Track | Out-Null
Write-Host "== Word geometry (COM) ==" -ForegroundColor Cyan
& (Join-Path $PSScriptRoot "word-geometry.ps1") -Docx $Docx -Out $wPath -Quiet | Out-Null

$S = Get-Content $sPath -Raw -Encoding UTF8 | ConvertFrom-Json
$W = Get-Content $wPath -Raw -Encoding UTF8 | ConvertFrom-Json

function Norm([string]$t) { ($t -replace '\s+', ' ').Trim().ToLowerInvariant() }

# Index each side's NON-EMPTY paragraphs by normalized text; a key usable for matching must be unique
# on both sides (so a delta is unambiguous). Empty spacers carry no text and are compared only by count.
function KeyMap($paras) {
  $m = @{}
  foreach ($p in $paras) {
    $k = Norm $p.text
    if (-not $k) { continue }
    if ($m.ContainsKey($k)) { $m[$k] = $null } else { $m[$k] = $p }  # null = ambiguous (appears twice)
  }
  $m
}
$sMap = KeyMap $S.paragraphs
$wMap = KeyMap $W.paragraphs

# Absolute document position (points): page-relative y is incomparable once the two sides paginate
# differently, so fold the page in. dY then reads as true cumulative drift - it starts near 0 at the
# top and grows downward as spacing/metrics diverge.
$sPH = if ($S.pageHeightPt) { [double]$S.pageHeightPt } else { 842.0 }
$wPH = if ($W.pageHeightPt) { [double]$W.pageHeightPt } else { 842.0 }
function AbsY($p, $ph) { ([double]$p.page - 1) * $ph + [double]$p.yPt }

$rows = New-Object System.Collections.Generic.List[object]
foreach ($k in $sMap.Keys) {
  $sp = $sMap[$k]; if ($null -eq $sp) { continue }
  if (-not $wMap.ContainsKey($k)) { continue }
  $wp = $wMap[$k]; if ($null -eq $wp) { continue }
  $rows.Add([pscustomobject]@{
      dPage = [int]$sp.page - [int]$wp.page
      dY    = [math]::Round((AbsY $sp $sPH) - (AbsY $wp $wPH), 1)
      dX    = [math]::Round([double]$sp.xPt - [double]$wp.xPt, 1)
      sList = [string]$sp.list
      wList = [string]$wp.list
      page  = [int]$wp.page
      wAbs  = AbsY $wp $wPH
      text  = [string]$wp.text
    }) | Out-Null
}

$sNonEmpty = @($S.paragraphs | Where-Object { Norm $_.text }).Count
$wNonEmpty = @($W.paragraphs | Where-Object { Norm $_.text }).Count
$matched = $rows.Count
$badPage = @($rows | Where-Object { $_.dPage -ne 0 })
$badY    = @($rows | Where-Object { [math]::Abs($_.dY) -gt $Tol })
$badX    = @($rows | Where-Object { [math]::Abs($_.dX) -gt $Tol })
$badList = @($rows | Where-Object { (Norm $_.sList) -ne (Norm $_.wList) })
$dys = @($rows | ForEach-Object { $_.dY } | Sort-Object)
$medDy = if ($dys.Count) { $dys[[int]($dys.Count / 2)] } else { 0 }

Write-Host ""
Write-Host "  pages       Scriptor $($S.pages)   Word $($W.pages)$(if($S.pages -ne $W.pages){'   <- MISMATCH'})" -ForegroundColor $(if ($S.pages -ne $W.pages) { 'Red' } else { 'Green' })
Write-Host "  paragraphs  Scriptor $($S.paragraphs.Count) ($sNonEmpty non-empty)   Word $($W.paragraphs.Count) ($wNonEmpty non-empty)"
Write-Host "  matched by text: $matched"
Write-Host ("  median dY (cumulative drift, absolute): {0} pt" -f $medDy) -ForegroundColor $(if ([math]::Abs($medDy) -gt $Tol) { 'Yellow' } else { 'Green' })
Write-Host "  divergent: page=$($badPage.Count)  dY>$Tol pt=$($badY.Count)  dX>$Tol pt=$($badX.Count)  list=$($badList.Count)" -ForegroundColor $(if ($badPage.Count + $badY.Count + $badX.Count + $badList.Count) { 'Yellow' } else { 'Green' })

# Where divergence first sets in (document order = Word's absolute Y): the root paragraph everything
# below inherits its drift from. Far more actionable than the largest delta (which is just the bottom).
$origin = @($rows | Sort-Object wAbs | Where-Object { [math]::Abs($_.dY) -gt $Tol } | Select-Object -First 1)
if ($origin.Count) {
  $o = $origin[0]
  Write-Host ("  drift begins at: dY={0} pt  dX={1} pt  page S/W diff={2}  `"{3}`"" -f $o.dY, $o.dX, $o.dPage, $o.text) -ForegroundColor Yellow
}

if ($badList.Count) {
  Write-Host ""
  Write-Host "  LIST-MARKER MISMATCHES (Word vs Scriptor):" -ForegroundColor Red
  $badList | Select-Object @{n = 'word'; e = { if ($_.wList) { $_.wList } else { '(none)' } } }, @{n = 'scriptor'; e = { if ($_.sList) { $_.sList } else { '(none)' } } }, text | Format-Table -AutoSize | Out-Host
}

Write-Host ""
Write-Host "  Worst vertical divergence (Scriptor - Word, points):"
$rows | Sort-Object { - [math]::Abs($_.dY) } | Select-Object -First $Top dPage, dY, dX, page, text | Format-Table -AutoSize | Out-Host

Write-Host "  full dumps: $sPath  |  $wPath"
