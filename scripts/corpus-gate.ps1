<#
.SYNOPSIS
  Conformance REGRESSION gate: byte-stable round-trip + schema-valid remodel over a corpus,
  diffed against a checked-in baseline. Fails (exit 1) if any document that used to round-trip or
  validate no longer does - so a change that reintroduces a fidelity regression breaks CI instead
  of silently passing. This is what freezes the "regressed-by-remodel 44 -> 0" campaign result.

.DESCRIPTION
  Three reference-free, deterministic checks (no Word / LibreOffice needed) over every .docx in the
  corpus:
    1. round-trip  - `scriptor roundtrip <corpus> --json`: parse -> re-serialize is byte-stable.
    2. resave      - `scriptor resave <corpus> --json`: open through the CRDT and save straight back
       with no edit; every part the model does not own must come back byte-identical and none may
       be dropped. This is the only layer that reaches to_docx_bytes, the path a browser host calls
       on Ctrl+S - check 1 is a container repack that never builds the model, and check 3 goes
       through export_docx, which replaces word/document.xml and copies the rest verbatim.
    3. remodel+schema - remodel each doc through the CRDT, then validate the output with the Open
       XML SDK (`tools/ooxml-validate`).
  Each is reduced to a {file -> status} map and diffed against the checked-in baseline under
  -BaselineDir. A document is a REGRESSION when it was good in the baseline (round-trip `stable`;
  resave `lossless`; schema `valid` or `unreadable`) and is now worse. IMPROVEMENTS (bad -> good) are reported but do
  not fail; run with -Update to fold the new reality into the baseline (e.g. after a fix, or after
  the corpus itself changes).

  The corpus is NOT vendored (the LibreOffice sw/qa docs are MPL / bug-tracker attachments - test
  inputs only), so point -Corpus at a local checkout, or set $env:SCRIPTOR_CORPUS. The baseline
  (our RESULTS, not the docs) is checked in and travels with the repo.

  Runs on Windows, Linux and macOS: PowerShell 7 (`pwsh`) plus the .NET SDK for the schema
  validator. Microsoft Word is NOT needed - only the Word-truth oracles (word-*.ps1) require it.

.PARAMETER Corpus       Folder of .docx to gate. Default: $env:SCRIPTOR_CORPUS.
.PARAMETER BaselineDir  Where the baseline JSONs live / are written.
.PARAMETER Update       Overwrite the baseline with the current results (accept the new state).
.PARAMETER WorkDir      Scratch dir for remodel outputs + interim JSON.
.PARAMETER Scriptor     Path to the built scriptor CLI (default: the release build).

.EXAMPLE
  # Gate the LO corpus against the checked-in baseline (CI / pre-merge).
  pwsh -File scripts/corpus-gate.ps1 -Corpus /path/to/sw/qa/extras/ooxmlexport/data
.EXAMPLE
  # After an intended fidelity change, refresh the baseline.
  pwsh -File scripts/corpus-gate.ps1 -Update
#>
param(
  [string]$Corpus = "",
  [string]$BaselineDir = (Join-Path $PSScriptRoot "../tests/baselines/lo-ooxmlexport"),
  [switch]$Update,
  # Cross-platform temp root: $env:TEMP is Windows-only, [System.IO.Path] resolves on every OS.
  [string]$WorkDir = (Join-Path ([System.IO.Path]::GetTempPath()) "scriptor-corpus-gate"),
  # The CLI only carries an .exe suffix on Windows. ($IsWindows is undefined in Windows PowerShell
  # 5.1, where the answer is always yes; it is a real boolean under pwsh on every platform.)
  [string]$Scriptor = (Join-Path $PSScriptRoot ("../target/release/scriptor" + $(if ($IsWindows -eq $false) { "" } else { ".exe" })))
)
$ErrorActionPreference = "Stop"

if (-not $Corpus) {
  if (-not $env:SCRIPTOR_CORPUS) {
    throw "no corpus: pass -Corpus <path to sw/qa/extras/ooxmlexport/data> or set `$env:SCRIPTOR_CORPUS"
  }
  $Corpus = $env:SCRIPTOR_CORPUS
}
if (-not (Test-Path $Corpus)) { throw "corpus not found: $Corpus (set -Corpus or `$env:SCRIPTOR_CORPUS)" }
if (-not (Test-Path $Scriptor)) { throw "build the CLI first: cargo build --release -p scriptor-cli" }
$validateProj = Join-Path $PSScriptRoot "../tools/ooxml-validate"
if (-not (Test-Path $validateProj)) { throw "ooxml-validate tool not found at $validateProj" }

$remodelDir = Join-Path $WorkDir "remodel"
foreach ($d in @($WorkDir, $remodelDir, $BaselineDir)) { New-Item -ItemType Directory -Force -Path $d | Out-Null }

# Write UTF-8 without a BOM (file names carry non-ASCII: Swedish / Hungarian / German). PS 5.1
# Out-File -Encoding utf8 emits a BOM; the .NET writer does not, keeping the checked-in JSON clean.
function Write-Json($obj, $path) {
  [System.IO.File]::WriteAllText($path, ($obj | ConvertTo-Json -Depth 6), (New-Object System.Text.UTF8Encoding($false)))
}
# {file -> status} from a tool's `docs` array.
function Status-Map($jsonPath) {
  $m = @{}
  foreach ($d in (Get-Content $jsonPath -Raw | ConvertFrom-Json).docs) { $m[$d.file] = $d.status }
  $m
}

Write-Host "== round-trip ($Corpus) ==" -ForegroundColor Cyan
$rtJson = Join-Path $WorkDir "roundtrip.json"
& $Scriptor roundtrip $Corpus --json $rtJson | Out-Null

Write-Host "== resave (browser save path) ==" -ForegroundColor Cyan
# `roundtrip` above is a container repack that never builds the model, and `remodel` below goes
# through export_docx, which replaces word/document.xml and copies every other part verbatim.
# Neither reaches to_docx_bytes - the path a browser host actually calls on Ctrl+S. This layer does:
# it opens each doc through the CRDT, saves it straight back with no edit, and fails any document
# that drops a part or changes one the save path does not own.
$rsJson = Join-Path $WorkDir "resave.json"
& $Scriptor resave $Corpus --json $rsJson | Out-Null

Write-Host "== remodel + schema-validate ==" -ForegroundColor Cyan
Remove-Item -Recurse -Force $remodelDir -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force $remodelDir | Out-Null
$docs = Get-ChildItem $Corpus -Filter *.docx | Where-Object { $_.Name -notlike '~$*' }
$i = 0
# A remodel can fail on a doc that is not a real .docx (encrypted CFB, an .odt with a .docx
# extension) - expected, and not our concern (those are invalid as originals too). Continue past
# it so one bad input can't abort the whole gate; the doc simply won't reach the validator.
$prevEap = $ErrorActionPreference
$ErrorActionPreference = "Continue"
foreach ($doc in $docs) {
  $i++
  if ($i % 200 -eq 0) { Write-Host "  remodeled $i/$($docs.Count)" }
  & $Scriptor remodel $doc.FullName (Join-Path $remodelDir $doc.Name) 2>$null | Out-Null
}
$ErrorActionPreference = $prevEap
$valJson = Join-Path $WorkDir "validation.json"
# Same stderr guard as the remodel loop: under EAP=Stop, a native command's stderr line (an MSBuild
# banner / NuGet restore notice on a cold day) becomes a terminating NativeCommandError with the
# `2>$null` redirect attached - killing the gate before the diff ever runs.
$prevEap = $ErrorActionPreference
$ErrorActionPreference = "Continue"
& dotnet run --project $validateProj -c Release -- $remodelDir --json $valJson --max-errors 0 2>$null | Out-Null
$ErrorActionPreference = $prevEap
if (-not (Test-Path $valJson)) { throw "ooxml-validate produced no $valJson - run the dotnet step by hand to see its error" }

# The "good" status for each check; anything else is a worse state.
$checks = @(
  @{ name = "roundtrip";  cur = $rtJson;  base = (Join-Path $BaselineDir "roundtrip.json");  good = @("stable") }
  @{ name = "resave";     cur = $rsJson;  base = (Join-Path $BaselineDir "resave.json");     good = @("lossless") }
  @{ name = "schema";     cur = $valJson; base = (Join-Path $BaselineDir "validation.json"); good = @("valid", "unreadable") }
)

if ($Update) {
  foreach ($c in $checks) {
    $map = Status-Map $c.cur
    $ordered = [ordered]@{}
    foreach ($k in ($map.Keys | Sort-Object)) { $ordered[$k] = $map[$k] }
    Write-Json ([pscustomobject]@{ corpus = (Split-Path $Corpus -Leaf); count = $map.Count; docs = $ordered }) $c.base
    Write-Host "updated baseline: $($c.base) ($($map.Count) docs)" -ForegroundColor Green
  }
  exit 0
}

$totalReg = 0; $totalImp = 0; $missing = @()
foreach ($c in $checks) {
  # A check added since the baseline was captured has nothing to diff against. Report it loudly and
  # carry on rather than throwing: a new layer must not take the whole gate down for everyone who
  # has not re-run -Update yet, and -Update refreshes EVERY baseline at once, so forcing it here
  # would quietly fold any unrelated regression into the ledger too. The run still fails on real
  # regressions in the checks that do have a baseline.
  if (-not (Test-Path $c.base)) {
    $missing += $c.name
    Write-Host ""
    Write-Host ("[{0}] NO BASELINE at {1}" -f $c.name, $c.base) -ForegroundColor Yellow
    Write-Host ("  not gated this run. Capture it with -Update once you have reviewed {0}." -f $c.cur) -ForegroundColor Yellow
    continue
  }
  $cur = Status-Map $c.cur
  $baseDocs = (Get-Content $c.base -Raw | ConvertFrom-Json).docs
  $base = @{}; foreach ($p in $baseDocs.PSObject.Properties) { $base[$p.Name] = $p.Value }

  $regressions = @(); $improvements = @()
  foreach ($f in $cur.Keys) {
    if (-not $base.ContainsKey($f)) { continue } # a new doc (corpus grew) - not a regression
    $wasGood = $c.good -contains $base[$f]
    $isGood = $c.good -contains $cur[$f]
    if ($wasGood -and -not $isGood) { $regressions += [pscustomobject]@{ doc = $f; from = $base[$f]; to = $cur[$f] } }
    elseif (-not $wasGood -and $isGood) { $improvements += [pscustomobject]@{ doc = $f; from = $base[$f]; to = $cur[$f] } }
  }
  Write-Host ""
  Write-Host ("[{0}] regressions: {1}   improvements: {2}" -f $c.name, $regressions.Count, $improvements.Count) `
    -ForegroundColor $(if ($regressions.Count) { "Red" } else { "Green" })
  $regressions | ForEach-Object { Write-Host ("  REGRESS  {0}  {1} -> {2}" -f $_.doc, $_.from, $_.to) -ForegroundColor Red }
  $improvements | ForEach-Object { Write-Host ("  improve  {0}  {1} -> {2}" -f $_.doc, $_.from, $_.to) -ForegroundColor DarkGray }
  $totalReg += $regressions.Count; $totalImp += $improvements.Count
}

Write-Host ""
if ($missing.Count) {
  Write-Host ("NOTE: {0} check(s) had no baseline and were not gated: {1}" -f $missing.Count, ($missing -join ", ")) -ForegroundColor Yellow
}
if ($totalReg -gt 0) {
  Write-Host "FAIL: $totalReg regression(s) vs baseline." -ForegroundColor Red
  exit 1
}
if ($totalImp -gt 0) {
  Write-Host "PASS with $totalImp improvement(s) - run with -Update to fold them into the baseline." -ForegroundColor Yellow
} else {
  Write-Host "PASS: corpus matches baseline, no regressions." -ForegroundColor Green
}
exit 0
