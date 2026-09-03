<#
.SYNOPSIS
  Dump real Microsoft Word's layout geometry for a .docx as JSON - the ground-truth oracle for
  Scriptor's layout fidelity (compare against `scriptor geometry`).

.DESCRIPTION
  Word is the only true source of "where does this paragraph actually land." This drives Word via COM,
  forces Print-Layout pagination, and reads each main-story paragraph's page + on-page position (in
  points, resolution-independent) plus the list marker Word computes ("1.", "1.1", a bullet) and its
  style. Pixel diffs are noisy (font rasterizers differ) and don't localize; this is exact and names
  the paragraph - it catches "this paragraph is 3 cm too high" and "the outline number is missing"
  directly.

  STABILITY. Word COM is famously leaky: Quit() does not reliably terminate a headless instance, and
  blindly killing WINWORD would close the user's open documents. So we (1) diff the WINWORD PID set
  across our CreateObject call to learn exactly which process is OURS, (2) suppress every dialog that
  could hang automation, and (3) in `finally`, Quit + release + force-kill ONLY our PID - never a
  pre-existing one.

.PARAMETER Docx  Path to the .docx.
.PARAMETER Out   JSON output path (default: %TEMP%\scriptor-geom\<name>.word.json).
.PARAMETER Quiet Suppress the human summary on stderr (JSON path still printed to stdout).

.EXAMPLE
  pwsh scripts/word-geometry.ps1 -Docx contract.docx
#>
param(
  [Parameter(Mandatory = $true)][string]$Docx,
  [string]$Out = "",
  [switch]$Quiet
)
$ErrorActionPreference = "Stop"

# WdInformation enum (the only members we read).
$wdActiveEndPageNumber           = 3
$wdHorizontalPositionRelToPage   = 5
$wdVerticalPositionRelToPage     = 6
$wdStatisticPages                = 2
$wdPrintView                     = 3
$wdCollapseStart                 = 1

$Docx = (Resolve-Path $Docx).Path
$name = [IO.Path]::GetFileNameWithoutExtension($Docx)
if (-not $Out) {
  $dir = Join-Path $env:TEMP "scriptor-geom"
  New-Item -ItemType Directory -Force -Path $dir | Out-Null
  $Out = Join-Path $dir "$name.word.json"
}

function WinwordPids { @(Get-Process WINWORD -ErrorAction SilentlyContinue | Select-Object -Expand Id) }

# Snapshot the user's existing Word processes up front. Anything that appears between here and the
# `finally` is ours (Word can spawn more than one process - one at CreateObject, sometimes another on
# Open) - we kill exactly those and never a pre-existing one. The only assumption is that the user
# isn't launching Word by hand during this ~1s run, which holds for a tool they invoke deliberately.
$before = WinwordPids
$word = New-Object -ComObject Word.Application

$doc = $null
try {
  $word.Visible = $false
  $word.DisplayAlerts = 0           # wdAlertsNone
  $word.ScreenUpdating = $false
  $word.Options.ConfirmConversions = $false
  $word.Options.CheckGrammarAsYouType = $false
  $word.Options.CheckSpellingAsYouType = $false
  $word.AutomationSecurity = 3      # msoAutomationSecurityForceDisable (no macros)

  # Open(FileName, ConfirmConversions=$false, ReadOnly=$true, AddToRecentFiles=$false).
  $doc = $word.Documents.Open($Docx, $false, $true, $false)

  # Position info is only meaningful in Print-Layout view + after pagination. A hidden instance still
  # has a (hidden) document window, so ActiveWindow normally works; guard it just in case.
  try { $doc.ActiveWindow.View.Type = $wdPrintView } catch {}
  $doc.Repaginate()

  $pages = 0
  try { $pages = [int]$doc.ComputeStatistics($wdStatisticPages) } catch {}

  $paras = [System.Collections.Generic.List[object]]::new()
  $i = 0
  foreach ($p in $doc.Paragraphs) {
    $rec = [ordered]@{ i = $i; page = $null; xPt = $null; yPt = $null; lines = $null; style = ""; list = ""; text = "" }
    try {
      $r = $p.Range
      # Visual (wrapped) line count for the paragraph - the wrap-fidelity signal (Phase 0).
      # wdStatisticLines = 1.
      try { $rec.lines = [int]$p.Range.ComputeStatistics(1) } catch {}
      $r.Collapse($wdCollapseStart)  # top-left of the paragraph's first line
      $rec.page = [int]$r.Information($wdActiveEndPageNumber)
      $rec.xPt = [math]::Round([double]$r.Information($wdHorizontalPositionRelToPage), 1)
      $rec.yPt = [math]::Round([double]$r.Information($wdVerticalPositionRelToPage), 1)
    } catch {}
    try { $rec.style = [string]$p.Style.NameLocal } catch {}
    try { $rec.list = [string]$p.Range.ListFormat.ListString } catch {}
    try {
      $t = [string]$p.Range.Text
      $t = ($t -replace "[\r\n\a\t\f\v]", " ").Trim()
      if ($t.Length -gt 60) { $t = $t.Substring(0, 60) }
      $rec.text = $t
    } catch {}
    $paras.Add([pscustomobject]$rec)
    $i++
  }

  $pageHeightPt = 0.0
  try { $pageHeightPt = [math]::Round([double]$doc.PageSetup.PageHeight, 1) } catch {}
  $result = [pscustomobject]@{
    docx         = $Docx
    source       = "word"
    version      = [string]$word.Version
    units        = "pt"
    pages        = $pages
    pageHeightPt = $pageHeightPt
    paragraphs   = $paras
  }
  $result | ConvertTo-Json -Depth 6 | Out-File -FilePath $Out -Encoding utf8

  if (-not $Quiet) {
    [Console]::Error.WriteLine("Word $($word.Version): $pages page(s), $($paras.Count) paragraph(s) -> $Out")
    $listed = @($paras | Where-Object { $_.list })
    [Console]::Error.WriteLine("  paragraphs Word numbered: $($listed.Count)")
  }
  $Out
}
finally {
  if ($doc) { try { $doc.Close($false) } catch {} }   # wdDoNotSaveChanges
  try { $word.Quit($false) } catch {}
  try { [void][Runtime.InteropServices.Marshal]::ReleaseComObject($word) } catch {}
  [GC]::Collect(); [GC]::WaitForPendingFinalizers(); [GC]::Collect()
  Start-Sleep -Milliseconds 300
  # Quit() does not reliably terminate a headless instance - force-kill every WINWORD that appeared
  # during our run (one or more), leaving the user's pre-existing instances untouched.
  foreach ($wp in @(WinwordPids | Where-Object { $_ -notin $before })) {
    Stop-Process -Id $wp -Force -ErrorAction SilentlyContinue
  }
}
