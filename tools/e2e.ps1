# End-to-end workflow tests (crates/app/tests/e2e_*.rs, crates/io/tests/e2e_*.rs).
# usage: pwsh tools/e2e.ps1 [light|heavy|all] [name-filter]
#   light — synthetic fixtures, always-on tests (debug build)
#   heavy — #[ignore] corpus tests (release; corpus files named by the corpus list,
#           override per test with DDC_E2E_* env vars; missing files are skipped)
#   all   — both. DDC_E2E_SEQS raises the randomized sequence count.
param([ValidateSet("light", "heavy", "all")][string]$Set = "light", [string]$Filter = "")
$cargo = Join-Path $env:USERPROFILE ".cargo\bin\cargo.exe"
$root = Split-Path -Parent $PSScriptRoot
$fail = $false

function Targets([string]$crate) {
    $dir = Join-Path $root "crates\$crate\tests"
    Get-ChildItem -Path $dir -Filter "e2e_*.rs" -File | ForEach-Object { @("--test", $_.BaseName) }
}

function Run([string]$crate, [string[]]$extra, [bool]$release) {
    $t = @(Targets $crate)
    if ($t.Count -eq 0) { return }
    $a = @("test", "-q", "-p", "ddc-$crate") + $t
    if ($release) { $a += "--release" }
    $a += "--"
    if ($Filter) { $a += $Filter }
    $a += $extra
    $out = & $cargo @a 2>&1 | Out-String
    $out -split "`n" | Select-String "test result|panicked|^error|-->|FAILED|e2e:|left:|right:" |
        Select-Object -First 40 | ForEach-Object { "[$crate] " + $_.Line.TrimEnd() }
    if ($LASTEXITCODE -ne 0) { $script:fail = $true }
}

Push-Location $root
try {
    foreach ($c in @("app", "io")) {
        if ($Set -in @("light", "all")) { Run $c @() $false }
        if ($Set -in @("heavy", "all")) { Run $c @("--ignored", "--nocapture") $true }
    }
} finally { Pop-Location }
if ($fail) { "E2E FAIL"; exit 1 } else { "E2E OK ($Set)" }
