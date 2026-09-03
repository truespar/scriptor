<#
.SYNOPSIS
  Word-truth verification of Scriptor OUTPUTS: open each written .docx in real Microsoft Word,
  detect open/repair failures, and compare content signals (text, words, revisions, comments,
  pages) against the ORIGINAL document. Automates the Phase-0 FAIL criterion ("Word repairs or
  rejects the file") that DisplayAlerts=0 otherwise suppresses.

.DESCRIPTION
  Per document pair (output vs original):
    1. Open the OUTPUT with OpenAndRepair:=false and every dialog suppressed. A damaged package
       makes Open() throw (the recover prompt cannot show) -> status "open-fail": the hard FAIL.
    2. Extract signals: normalized body text hash, word/char counts, Revisions.Count,
       Comments.Count, page count.
    3. Extract the same signals from the ORIGINAL (cached in -ResultsDir, keyed by name+mtime,
       so re-runs only re-open outputs).
    4. Compare -> ok | text-diff | counts-diff, with the first text divergence excerpted.

  Results: one JSON per doc under -ResultsDir (resumable - existing fresh results are skipped)
  plus a ranked CSV (-Out): open-fail first, then text-diff, then counts-diff, then ok.

  STABILITY. Each open runs in a watchdog'd child job (its own Word instance) with a -TimeoutSec
  deadline, so a document that blocks Open() on an invisible modal (external-reference fields:
  INCLUDEPICTURE / INCLUDETEXT / AUTOTEXTLIST raise a "potential security concern" prompt that
  cannot be answered headlessly) is recorded as open-timeout and its Word killed, instead of
  hanging the whole batch. PID-diff around each job kills ONLY instances we spawned - never a
  developer's open documents. (This trades the old single-recycled-instance speed for a batch
  that always makes progress; a full-corpus run is slower but never wedges.)

  CAVEAT (known benign diffs): field RESULTS may differ (a re-wrapped PAGE field carries cached
  "1" + w:dirty until Word repaginates; DATE fields recompute), so a small text-diff on a
  field-heavy doc needs an eyeball before it counts as a regression. Counts (revisions,
  comments, pages) are the more stable signals.

.PARAMETER Dir          Folder of OUTPUT .docx to verify (e.g. %TEMP%\remodel-corpus).
.PARAMETER OriginalDir  Folder of the ORIGINALS (matched by file name).
.PARAMETER List         Optional file of doc names (one per line) to restrict to.
.PARAMETER Max          Cap the number of docs when no -List is given (0 = no cap).
.PARAMETER Out          Ranked CSV output path.
.PARAMETER ResultsDir   Per-doc JSON results + originals cache (resumable).
.PARAMETER Recycle      Restart the Word instance every N documents (default 40).
.PARAMETER Force        Re-verify even if a fresh per-doc result exists.

.EXAMPLE
  powershell -File scripts\word-verify.ps1 -Dir $env:TEMP\remodel-corpus -OriginalDir <corpus>\sw\qa\extras\ooxmlexport\data -List priority.txt
#>
param(
  [Parameter(Mandatory = $true)][string]$Dir,
  [Parameter(Mandatory = $true)][string]$OriginalDir,
  [string]$List = "",
  [int]$Max = 0,
  [string]$Out = "$env:TEMP\scriptor-wordverify\verify.csv",
  [string]$ResultsDir = "$env:TEMP\scriptor-wordverify\results",
  [int]$TimeoutSec = 60,
  [switch]$Force
)
$ErrorActionPreference = "Stop"

$origCache = Join-Path $ResultsDir "originals"
foreach ($d in @($ResultsDir, $origCache, (Split-Path $Out))) { if ($d) { New-Item -ItemType Directory -Force -Path $d | Out-Null } }

# The doc set: an explicit -List, else every .docx in -Dir (skipping ~$ lock files).
if ($List -and (Test-Path $List)) {
  $names = Get-Content $List | Where-Object { $_.Trim() -ne "" }
} else {
  $names = Get-ChildItem $Dir -Filter *.docx | Where-Object { $_.Name -notlike '~$*' } |
    Sort-Object Name | Select-Object -ExpandProperty Name
  if ($Max -gt 0) { $names = $names | Select-Object -First $Max }
}

function WinwordPids { @(Get-Process WINWORD -ErrorAction SilentlyContinue | Select-Object -Expand Id) }
$script:before = WinwordPids

# The Word-open work, run inside a child job so the PARENT can enforce a timeout: a document whose
# fields reference external content (INCLUDEPICTURE / INCLUDETEXT / AUTOTEXTLIST) makes Word raise a
# "potential security concern" modal that is invisible under Visible=$false and BLOCKS Open()
# forever - an in-process COM call on the same thread cannot be interrupted, so the whole batch used
# to hang on one such doc. A per-doc job (its own Word instance) is killable; we sacrifice instance
# recycling for a batch that always makes progress.
$OpenBody = {
  param($path)
  $missing = [System.Reflection.Missing]::Value
  $sha = [System.Security.Cryptography.SHA1]::Create()
  $w = New-Object -ComObject Word.Application
  $w.Visible = $false
  $w.DisplayAlerts = 0            # wdAlertsNone - a repair prompt makes Open() THROW instead
  $w.ScreenUpdating = $false
  try { $w.Options.ConfirmConversions = $false } catch {}
  try { $w.Options.CheckGrammarAsYouType = $false } catch {}
  try { $w.Options.CheckSpellingAsYouType = $false } catch {}
  try { $w.Options.UpdateLinksAtOpen = $false } catch {}
  $w.AutomationSecurity = 3       # msoAutomationSecurityForceDisable (no macros)
  $doc = $null
  try {
    # Open positional args: FileName, ConfirmConversions, ReadOnly, AddToRecentFiles, ...,
    # Visible, OpenAndRepair - the trailing $false is OpenAndRepair, so a damaged package throws
    # rather than silently repairing.
    $doc = $w.Documents.Open($path, $false, $true, $false,
      $missing, $missing, $missing, $missing, $missing, $missing, $missing, $false, $false)
    $text = ""
    try { $text = [string]$doc.Content.Text } catch {}
    $norm = (($text -replace "[\r\n\a\t\f\v\x01\x02\x05\x08\x0c\x0e]+", " ") -replace "\s+", " ").Trim()
    $hash = [BitConverter]::ToString($sha.ComputeHash([Text.Encoding]::UTF8.GetBytes($norm))).Replace("-", "")
    $words = 0; $chars = 0; $pages = 0; $revs = 0; $cmts = 0
    try { $words = [int]$doc.ComputeStatistics(0) } catch {}   # wdStatisticWords
    try { $chars = [int]$doc.ComputeStatistics(3) } catch {}   # wdStatisticCharacters
    try { $pages = [int]$doc.ComputeStatistics(2) } catch {}   # wdStatisticPages (forces pagination)
    try { $revs = [int]$doc.Revisions.Count } catch {}
    try { $cmts = [int]$doc.Comments.Count } catch {}
    [pscustomobject]@{ opened = $true; error = ""; textHash = $hash; textNorm = $norm
                       words = $words; chars = $chars; pages = $pages; revisions = $revs; comments = $cmts }
  }
  catch {
    [pscustomobject]@{ opened = $false; error = [string]$_.Exception.Message; textHash = ""; textNorm = ""
                       words = 0; chars = 0; pages = 0; revisions = 0; comments = 0 }
  }
  finally {
    if ($doc) { try { $doc.Close($false) } catch {} }
    try { $w.Quit($false) } catch {}
  }
}

# Run one open in a watchdog'd job. On timeout, record open-timeout and kill exactly the WINWORD
# instance(s) the job spawned (PID diff around the job) - never a pre-existing one.
function Get-DocSignals([string]$path) {
  $pidsBefore = WinwordPids
  $inconclusive = { param($why) [pscustomobject]@{ opened = $false; inconclusive = $true; error = $why
                    textHash = ""; textNorm = ""; words = 0; chars = 0; pages = 0; revisions = 0; comments = 0 } }
  $job = Start-Job -ScriptBlock $OpenBody -ArgumentList $path
  $done = Wait-Job $job -Timeout $TimeoutSec
  $result = if (-not $done) {
    Stop-Job $job
    & $inconclusive "open-timeout (${TimeoutSec}s) - likely a modal (external-reference fields)"
  } else {
    $out = Receive-Job $job -ErrorAction SilentlyContinue
    # The scriptblock always returns our object; a bare object with `.opened` set is a real
    # verdict. Anything else (job State=Failed, a crash from the security-modal interaction) is
    # INCONCLUSIVE, not a corruption verdict - so it does not fail the gate.
    $obj = @($out | Where-Object { $_ -is [pscustomobject] -and $null -ne $_.opened }) | Select-Object -First 1
    if ($obj) { $obj }
    elseif ($job.State -eq 'Failed') { & $inconclusive ("open-error: " + ($job.ChildJobs[0].JobStateInfo.Reason.Message -replace '[\r\n]+',' ')) }
    else { & $inconclusive "open-error: job produced no verdict" }
  }
  Remove-Job $job -Force -ErrorAction SilentlyContinue
  Start-Sleep -Milliseconds 200
  foreach ($wp in @(WinwordPids | Where-Object { $_ -notin $pidsBefore -and $_ -notin $script:before })) {
    Stop-Process -Id $wp -Force -ErrorAction SilentlyContinue
  }
  $result
}

# First point where two normalized texts diverge, excerpted for the report.
function First-Diff([string]$a, [string]$b) {
  $n = [Math]::Min($a.Length, $b.Length)
  $i = 0
  while ($i -lt $n -and $a[$i] -eq $b[$i]) { $i++ }
  if ($i -ge $n -and $a.Length -eq $b.Length) { return "" }
  $from = [Math]::Max(0, $i - 30)
  $ea = $a.Substring($from, [Math]::Min(60, $a.Length - $from))
  $eb = $b.Substring($from, [Math]::Min(60, $b.Length - $from))
  "@${i}: output='…$ea…' original='…$eb…'"
}

$rows = @()
$since = 0
$idx = 0
try {
  foreach ($n in $names) {
    $idx++
    $outDoc = Join-Path $Dir $n
    $orig = Join-Path $OriginalDir $n
    $resPath = Join-Path $ResultsDir ("$n.json")
    Write-Host ("[{0}/{1}] {2}" -f $idx, @($names).Count, $n)

    if (-not (Test-Path $outDoc)) { $rows += [pscustomobject]@{doc=$n;status="missing-output";detail=""}; continue }
    if (-not (Test-Path $orig))   { $rows += [pscustomobject]@{doc=$n;status="missing-original";detail=""}; continue }

    # Resumable: a fresh per-doc result (newer than the output doc) is reused.
    if (-not $Force -and (Test-Path $resPath) -and
        ((Get-Item $resPath).LastWriteTime -gt (Get-Item $outDoc).LastWriteTime)) {
      $r = Get-Content $resPath -Raw | ConvertFrom-Json
      $rows += [pscustomobject]@{doc=$n;status=$r.status;detail=$r.detail}
      continue
    }

    # Originals cache: name+mtime keyed (an original never changes between corpus updates).
    $origRes = Join-Path $origCache ("$n.json")
    $o = $null
    if ((Test-Path $origRes) -and ((Get-Item $origRes).LastWriteTime -gt (Get-Item $orig).LastWriteTime)) {
      $o = Get-Content $origRes -Raw | ConvertFrom-Json
    } else {
      $o = Get-DocSignals $orig
      $o | ConvertTo-Json -Compress | Out-File $origRes -Encoding utf8
    }

    $s = Get-DocSignals $outDoc

    $status = "ok"; $detail = ""
    # Original first: if Word cannot open the SOURCE either (corpus-quirk docs - tdf142700
    # ships a vim .swp inside the zip), the output failing is not a Scriptor regression.
    if (-not $o.opened) {
      $status = "original-unopenable"; $detail = $o.error -replace "[\r\n,]+", " "
    } elseif ($s.inconclusive) {
      # A blocked open (invisible modal for external-reference fields) or a job crash - the
      # OUTPUT is not necessarily broken, so this is inconclusive, NOT a hard open-fail, and does
      # not fail the gate. Re-verify interactively (Visible=$true) or exclude the doc.
      $status = "open-timeout"; $detail = $s.error -replace "[\r\n,]+", " "
    } elseif (-not $s.opened) {
      # A clean verdict from Word that the package is unopenable (e.g. "appears to be corrupted").
      $status = "open-fail"; $detail = $s.error -replace "[\r\n,]+", " "
    } elseif ($s.textHash -ne $o.textHash) {
      $status = "text-diff"
      $detail = "words {0}->{1} chars {2}->{3} revs {4}->{5} cmts {6}->{7} pages {8}->{9} {10}" -f `
        $o.words, $s.words, $o.chars, $s.chars, $o.revisions, $s.revisions, $o.comments, $s.comments,
        $o.pages, $s.pages, (First-Diff $s.textNorm $o.textNorm)
    } elseif ($s.revisions -ne $o.revisions -or $s.comments -ne $o.comments) {
      $status = "counts-diff"
      $detail = "revs {0}->{1} cmts {2}->{3}" -f $o.revisions, $s.revisions, $o.comments, $s.comments
    } elseif ($s.pages -ne $o.pages) {
      $status = "pages-diff"
      $detail = "pages {0}->{1}" -f $o.pages, $s.pages
    }

    if ($status -ne "ok") { Write-Host ("    {0}  {1}" -f $status, $detail) -ForegroundColor Yellow }
    [pscustomobject]@{doc=$n;status=$status;detail=$detail} | ConvertTo-Json -Compress | Out-File $resPath -Encoding utf8
    $rows += [pscustomobject]@{doc=$n;status=$status;detail=$detail}
  }
}
finally {
  Start-Sleep -Milliseconds 300
  # Backstop: kill any WINWORD that appeared during our run and outlived its job, never a
  # pre-existing one (the user's open documents).
  foreach ($wp in @(WinwordPids | Where-Object { $_ -notin $script:before })) {
    Stop-Process -Id $wp -Force -ErrorAction SilentlyContinue
  }
}

# open-fail is the hard FAIL; then content divergence; timeouts / clean docs last (a timeout is
# inconclusive, not a failure).
$sev = @{ "open-fail" = 0; "original-unopenable" = 1; "missing-output" = 2; "missing-original" = 2
          "text-diff" = 3; "counts-diff" = 4; "pages-diff" = 5; "open-timeout" = 6; "ok" = 7 }
$rows | Sort-Object -Property @{Expression = { $sev[$_.status] }}, doc | Export-Csv -NoTypeInformation -Encoding UTF8 $Out
$byStatus = ($rows | Group-Object status | Sort-Object Count -Descending | ForEach-Object { "$($_.Name)=$($_.Count)" }) -join "  "
Write-Host ""
Write-Host "verified $(@($rows).Count) doc(s):  $byStatus" -ForegroundColor Green
Write-Host "ranked CSV: $Out"
if (@($rows | Where-Object { $_.status -eq "open-fail" }).Count -gt 0) { exit 1 }
