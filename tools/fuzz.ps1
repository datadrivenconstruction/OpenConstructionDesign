# SPDX-License-Identifier: AGPL-3.0-only
# SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
# Mutation fuzz of every untrusted-input reader (ignored tests). Prints FUZZ lines and failures.
# usage: pwsh tools/fuzz.ps1 [-Minutes N] [-Seed S]
# Failing inputs are saved to %TEMP%\ddc-fuzz-fail-<target>-<n>.bin; add them as regression tests.
param([double]$Minutes = 1, [int]$Seed = 1)
. (Join-Path $PSScriptRoot "_lib.ps1")
if (-not $env:CARGO_BUILD_JOBS) { $env:CARGO_BUILD_JOBS = "1" }
Assert-Disk
$targets = 9
$env:DDC_FUZZ_SECS = [string][math]::Max(1, [math]::Ceiling($Minutes * 60 / $targets))
$env:DDC_FUZZ_SEED = [string]$Seed
Push-Location $script:Root
$fail = $false
foreach ($run in @(@("ddc-io", "fuzz"), @("ddc-app", "fuzz_units"))) {
    $out = & $script:Cargo test --profile fuzz -p $run[0] --test $run[1] -- --ignored --nocapture --test-threads=1 2>&1 | Out-String
    if ($LASTEXITCODE -ne 0) { $fail = $true }
    $out -split "`n" | Select-String "^FUZZ |panicked|^error|FAILED|saved to" | Select-Object -First 40 | ForEach-Object { $_.Line.TrimEnd() }
}
Pop-Location
if ($fail) { "FUZZ FAIL"; exit 1 } else { "FUZZ OK" }
