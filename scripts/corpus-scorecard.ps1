<#
.SYNOPSIS
  Batch fidelity scorecard: run a whole folder of .docx through Scriptor + Word and rank every doc by
  how far Scriptor's layout diverges from Word's. Built on the same geometry oracle as
  `geometry-diff.ps1`, but over a corpus, with a "does it even open" column up front.

.DESCRIPTION
  Three passes:
    1. Scriptor - `scriptor geometry` per doc (the engine's own caret geometry). A non-zero exit /
       panic = the doc DOESN'T OPEN in Scriptor (status `import-fail`); its error is recorded and the
       doc is skipped for the geometry diff. This catches crashes like the bookmark out-of-bounds.
    2. Word - ONE headless WINWORD opens each doc that imported, repaginates, and dumps each
       paragraph's page + position (points). One instance for the whole batch (fast); killed at the
       end (only the PIDs we spawned, never the user's open Word).
    3. Compare - pair paragraphs by normalized text and score: pages match, median cumulative dY
       (absolute, points), # paragraphs off by >Tol in dX, # list-marker mismatches.

  Output: a ranked console table (worst first) + a CSV. Sort key puts import-fails first, then
  page-count mismatches, then the largest |median dY|.

.PARAMETER Dir       Folder to scan for .docx (recursively).
.PARAMETER Pattern   Optional regex on the FILE NAME to pre-filter (e.g. layout-ish fixtures).
.PARAMETER Max       Cap the number of docs (after Pattern), for a quick smoke pass. 0 = no cap.
.PARAMETER Tol       dX/dY divergence threshold in points (default 2).
.PARAMETER Out       CSV output path (default %TEMP%\scriptor-geom\scorecard.csv).
.PARAMETER Fresh     Ignore cached per-doc JSON and re-run everything.
.PARAMETER WordEvery Recycle the WINWORD instance every N opened docs (default 150).

.NOTES
  WINDOWS ONLY - it drives Word over COM. Build the CLI first
  (`cargo build -p scriptor-cli --release`); if linking fails with LNK1104, dot-source
  scripts/dev-shell.ps1 to load a complete MSVC environment.
  Per-doc results are cached under the temp dir: a rebuilt CLI re-runs only the fast Scriptor pass
  (Word dumps are reused), and a crashed run resumes where it left off.

.EXAMPLE
  # quick smoke pass
  scripts\corpus-scorecard.ps1 -Dir <corpus>\sw\qa\extras\ooxmlexport\data -Max 50
.EXAMPLE
  # full export corpus, ranked CSV - run in the background, ~40 min the first time
  scripts\corpus-scorecard.ps1 -Dir <corpus>\sw\qa\extras\ooxmlexport\data -Out scorecard-full.csv
#>
param(
  [Parameter(Mandatory = $true)][string]$Dir,
  [string]$Pattern = "",
  [int]$Max = 0,
  [double]$Tol = 2.0,
  [string]$Out = "",
  # Force a full re-run, ignoring cached per-doc JSON. Default: reuse a cached result when it's still
  # valid (Scriptor JSON newer than the exe; Word JSON newer than the docx), so re-scoring after a
  # Scriptor rebuild only re-runs the fast Scriptor pass + Word for genuinely-changed docs.
  [switch]$Fresh,
  # Recreate the WINWORD instance every N opened docs, to bound memory growth over a large corpus.
  [int]$WordEvery = 150
)
$ErrorActionPreference = "Stop"

$scriptor = Join-Path $PSScriptRoot "../target/release/scriptor.exe"
if (-not (Test-Path $scriptor)) { throw "build the CLI first: cargo build -p scriptor-cli --release" }
$work = Join-Path $env:TEMP "scriptor-geom\corpus"
New-Item -ItemType Directory -Force -Path $work | Out-Null
if (-not $Out) { $Out = Join-Path $env:TEMP "scriptor-geom\scorecard.csv" }
$null = New-Item -ItemType Directory -Force -Path (Split-Path $Out)
$exeTime = (Get-Item $scriptor).LastWriteTime
# A cached output file is reusable when it exists and post-dates every input that could change it.
function Fresh-Enough([string]$out, [datetime[]]$inputs) {
  if ($Fresh -or -not (Test-Path $out)) { return $false }
  $t = (Get-Item $out).LastWriteTime
  foreach ($i in $inputs) { if ($t -le $i) { return $false } }
  $true
}

# ── doc list ──────────────────────────────────────────────────────────────────
$docs = @(Get-ChildItem -Path $Dir -Filter *.docx -Recurse -File | Sort-Object Name)
if ($Pattern) { $docs = @($docs | Where-Object { $_.Name -match $Pattern }) }
if ($Max -gt 0 -and $docs.Count -gt $Max) { $docs = @($docs[0..($Max - 1)]) }
Write-Host "scoring $($docs.Count) docs from $Dir" -ForegroundColor Cyan

function Norm([string]$t) { ($t -replace '\s+', ' ').Trim().ToLowerInvariant() }
function KeyMap($paras) {
  $m = @{}
  foreach ($p in $paras) {
    $k = Norm $p.text
    if (-not $k) { continue }
    if ($m.ContainsKey($k)) { $m[$k] = $null } else { $m[$k] = $p }
  }
  $m
}

# ── pass 1: Scriptor geometry (+ import-survives) ───────────────────────────────
$rows = New-Object System.Collections.Generic.List[object]
$okForWord = New-Object System.Collections.Generic.List[object]
$i = 0
foreach ($d in $docs) {
  $i++
  $sJson = Join-Path $work ($d.BaseName + ".scriptor.json")
  $errF = Join-Path $work "err.txt"
  $row = [ordered]@{
    doc = $d.Name; status = ''; pagesS = ''; pagesW = ''; pageOK = ''
    matched = ''; medianDy = ''; dxOver = ''; listMis = ''; lineMis = ''; note = ''
  }
  # Reuse a cached Scriptor result when it post-dates both the exe and the doc (a cached JSON means it
  # imported last time - an import-fail leaves none, so it always re-runs and re-checks after a fix).
  if (Fresh-Enough $sJson @($exeTime, $d.LastWriteTime)) {
    Write-Host ("[{0,3}/{1}] scriptor: {2} (cached)" -f $i, $docs.Count, $d.Name)
    $okForWord.Add([pscustomobject]@{ doc = $d; sJson = $sJson; row = $row }) | Out-Null
    continue
  }
  Write-Host ("[{0,3}/{1}] scriptor: {2}" -f $i, $docs.Count, $d.Name)
  if (Test-Path $sJson) { Remove-Item $sJson -Force }
  $p = Start-Process -FilePath $scriptor -ArgumentList @('geometry', $d.FullName, '--out', $sJson, '--track', 'all') `
    -NoNewWindow -Wait -PassThru -RedirectStandardError $errF -RedirectStandardOutput (Join-Path $work "out.txt")
  if ($p.ExitCode -ne 0 -or -not (Test-Path $sJson)) {
    $msg = (Get-Content $errF -Raw -ErrorAction SilentlyContinue)
    $line = ($msg -split "`n" | Where-Object { $_ -match '\S' } | Select-Object -First 1)
    $row.status = 'import-fail'
    $row.note = ($line -replace '\s+', ' ').Trim()
    if ($row.note.Length -gt 80) { $row.note = $row.note.Substring(0, 80) }
    $rows.Add([pscustomobject]$row) | Out-Null
  }
  else {
    $okForWord.Add([pscustomobject]@{ doc = $d; sJson = $sJson; row = $row }) | Out-Null
  }
}

# ── pass 2: Word geometry (restarted periodically to bound memory) ──────────────
$before = @(Get-Process WINWORD -ErrorAction SilentlyContinue | Select-Object -Expand Id)
function New-Word {
  $w = New-Object -ComObject Word.Application
  $w.Visible = $false; $w.DisplayAlerts = 0; $w.ScreenUpdating = $false
  $w.Options.ConfirmConversions = $false
  $w.AutomationSecurity = 3
  $w
}
$word = New-Word
$opened = 0  # docs actually opened in Word this run (cache hits don't count toward the restart cadence)
try {
  $i = 0
  foreach ($item in $okForWord) {
    $i++
    $d = $item.doc
    $wJson = Join-Path $work ($d.BaseName + ".word.json")
    $item | Add-Member -NotePropertyName wJson -NotePropertyValue $wJson -Force
    # A cached Word dump is valid as long as the doc itself hasn't changed (Word's layout of a fixed
    # file is deterministic across runs) - reuse it so a re-score only re-drives Word for new docs.
    if (Fresh-Enough $wJson @($d.LastWriteTime)) {
      Write-Host ("[{0,3}/{1}] word:     {2} (cached)" -f $i, $okForWord.Count, $d.Name)
      continue
    }
    # Recycle WINWORD every $WordEvery opens to stop memory creep over a large corpus.
    if ($opened -gt 0 -and ($opened % $WordEvery) -eq 0) {
      try { $word.Quit($false) } catch {}
      try { [void][Runtime.InteropServices.Marshal]::ReleaseComObject($word) } catch {}
      [GC]::Collect(); [GC]::WaitForPendingFinalizers()
      $word = New-Word
    }
    $opened++
    Write-Host ("[{0,3}/{1}] word:     {2}" -f $i, $okForWord.Count, $d.Name)
    $doc = $null
    try {
      $doc = $word.Documents.Open($d.FullName, $false, $true, $false)
      try { $doc.ActiveWindow.View.Type = 3 } catch {}
      $doc.Repaginate()
      $pages = 0; try { $pages = [int]$doc.ComputeStatistics(2) } catch {}
      $paras = [System.Collections.Generic.List[object]]::new()
      $idx = 0
      foreach ($pa in $doc.Paragraphs) {
        $rec = [ordered]@{ i = $idx; page = $null; xPt = $null; yPt = $null; lines = $null; list = ""; text = "" }
        # Visual (wrapped) line count - the wrap-fidelity signal (Phase 0). wdStatisticLines = 1, on the
        # FULL paragraph range (before the collapse below).
        try { $rec.lines = [int]$pa.Range.ComputeStatistics(1) } catch {}
        try {
          $r = $pa.Range; $r.Collapse(1)
          $rec.page = [int]$r.Information(3)
          $rec.xPt = [math]::Round([double]$r.Information(5), 1)
          $rec.yPt = [math]::Round([double]$r.Information(6), 1)
        } catch {}
        try { $rec.list = [string]$pa.Range.ListFormat.ListString } catch {}
        try {
          $t = ([string]$pa.Range.Text -replace "[\r\n\a\t\f\v]", " ").Trim()
          if ($t.Length -gt 60) { $t = $t.Substring(0, 60) }
          $rec.text = $t
        } catch {}
        $paras.Add([pscustomobject]$rec) | Out-Null
        $idx++
      }
      $ph = 0.0; try { $ph = [math]::Round([double]$doc.PageSetup.PageHeight, 1) } catch {}
      ([pscustomobject]@{ pages = $pages; pageHeightPt = $ph; paragraphs = $paras } |
        ConvertTo-Json -Depth 6) | Out-File -FilePath $wJson -Encoding utf8
      $doc.Close($false)
    }
    catch {
      if ($doc) { try { $doc.Close($false) } catch {} }
      $item.row.status = 'word-fail'
      $item.row.note = ($_.Exception.Message -replace '\s+', ' ').Trim()
    }
  }
}
finally {
  try { $word.Quit($false) } catch {}
  try { [void][Runtime.InteropServices.Marshal]::ReleaseComObject($word) } catch {}
  [GC]::Collect(); [GC]::WaitForPendingFinalizers(); [GC]::Collect(); Start-Sleep -Milliseconds 300
  foreach ($wp in @(Get-Process WINWORD -ErrorAction SilentlyContinue | Select-Object -Expand Id | Where-Object { $_ -notin $before })) {
    Stop-Process -Id $wp -Force -ErrorAction SilentlyContinue
  }
}

# ── pass 3: compare ─────────────────────────────────────────────────────────────
foreach ($item in $okForWord) {
  $row = $item.row
  if ($row.status -eq 'word-fail') { $rows.Add([pscustomobject]$row) | Out-Null; continue }
  try {
    $S = Get-Content $item.sJson -Raw -Encoding UTF8 | ConvertFrom-Json
    $W = Get-Content $item.wJson -Raw -Encoding UTF8 | ConvertFrom-Json
  }
  catch { $row.status = 'compare-fail'; $rows.Add([pscustomobject]$row) | Out-Null; continue }
  $sMap = KeyMap $S.paragraphs
  $wMap = KeyMap $W.paragraphs
  $sPH = if ($S.pageHeightPt) { [double]$S.pageHeightPt } else { 842.0 }
  $wPH = if ($W.pageHeightPt) { [double]$W.pageHeightPt } else { 842.0 }
  $dys = New-Object System.Collections.Generic.List[double]
  $dxOver = 0; $listMis = 0; $lineMis = 0
  foreach ($k in $sMap.Keys) {
    $sp = $sMap[$k]; if ($null -eq $sp) { continue }
    if (-not $wMap.ContainsKey($k)) { continue }
    $wp = $wMap[$k]; if ($null -eq $wp) { continue }
    $dy = (([double]$sp.page - 1) * $sPH + [double]$sp.yPt) - (([double]$wp.page - 1) * $wPH + [double]$wp.yPt)
    $dys.Add([math]::Round($dy, 1)) | Out-Null
    if ([math]::Abs([double]$sp.xPt - [double]$wp.xPt) -gt $Tol) { $dxOver++ }
    if ((Norm $sp.list) -ne (Norm $wp.list)) { $listMis++ }
    # Wrap fidelity (Phase 0 metric): paragraphs whose visual line count differs from Word - but ONLY
    # where BOTH sides report a real count (>=1). Word's ComputeStatistics(wdStatisticLines) returns 0
    # for table-cell + certain special paragraphs (it counts the main text story, not cell bodies), so
    # those are UNMEASURABLE by this oracle, not divergent. Counting them (S=1 vs W=0) inflated the
    # metric ~6x with table-cell noise (2389 of 2777 raw diffs) and masked the ~388 real wrap diffs.
    if ($null -ne $sp.lines -and $null -ne $wp.lines -and `
        [int]$sp.lines -gt 0 -and [int]$wp.lines -gt 0 -and [int]$sp.lines -ne [int]$wp.lines) { $lineMis++ }
  }
  $sorted = @($dys | Sort-Object)
  $med = if ($sorted.Count) { $sorted[[int]($sorted.Count / 2)] } else { 0 }
  $row.status = 'ok'
  $row.pagesS = [int]$S.pages; $row.pagesW = [int]$W.pages
  $row.pageOK = ($S.pages -eq $W.pages)
  $row.matched = $dys.Count
  $row.medianDy = $med
  $row.dxOver = $dxOver
  $row.listMis = $listMis
  $row.lineMis = $lineMis
  $rows.Add([pscustomobject]$row) | Out-Null
}

# ── rank + report ───────────────────────────────────────────────────────────────
# Worst first: import/word/compare failures, then page mismatches, then |median dY|.
function Score($r) {
  if ($r.status -ne 'ok') { return [double]1e9 }
  $s = 0.0
  if ($r.pageOK -eq $false) { $s += 1e6 }
  $s += [math]::Abs([double]$r.medianDy) * 1000 + [double]$r.dxOver * 10 + [double]$r.listMis
  $s
}
$ranked = @($rows | Sort-Object @{ Expression = { Score $_ }; Descending = $true }, doc)
$ranked | Export-Csv -Path $Out -NoTypeInformation -Encoding UTF8

$nFail = @($rows | Where-Object { $_.status -ne 'ok' }).Count
$nPage = @($rows | Where-Object { $_.status -eq 'ok' -and $_.pageOK -eq $false }).Count
$okRows = @($rows | Where-Object { $_.status -eq 'ok' })
$medAll = if ($okRows.Count) { [math]::Round((@($okRows | ForEach-Object { [math]::Abs([double]$_.medianDy) } | Sort-Object))[[int]($okRows.Count / 2)], 1) } else { 0 }
# Wrap-fidelity baseline (Phase 0): paragraphs whose MEASURABLE visual line count differs from Word
# (both sides >=1 line; table-cell/special W=0 paragraphs excluded - see the compare loop), and how
# many docs have any such divergence - the gate for the line-wrapping-fidelity work.
$lineMisTotal = (@($okRows | ForEach-Object { [int]$_.lineMis }) | Measure-Object -Sum).Sum
$lineMisDocs = @($okRows | Where-Object { [int]$_.lineMis -gt 0 }).Count

Write-Host ""
Write-Host "  scored $($rows.Count): $($okRows.Count) ok, $nFail failed, $nPage page-mismatch" -ForegroundColor Cyan
Write-Host "  median |median dY| across ok docs: $medAll pt"
Write-Host "  wrap divergence (measurable, both sides >=1 line): $lineMisTotal paragraphs over $lineMisDocs docs differ from Word's line count" -ForegroundColor Cyan
Write-Host ""
$ranked | Select-Object -First 40 doc, status, pagesS, pagesW, pageOK, matched, medianDy, dxOver, listMis, lineMis, note |
  Format-Table -AutoSize | Out-Host
Write-Host "  full scorecard: $Out"
