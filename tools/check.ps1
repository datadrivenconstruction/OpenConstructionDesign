# Compact check: fmt + tests (+ clippy if installed). Prints only results and errors.
# usage: pwsh tools/check.ps1 [crate|assets] [test-filter]
param([string]$Crate = "", [string]$Filter = "", [switch]$Live)
if ($Crate -eq "assets") { python -I (Join-Path $PSScriptRoot "assets_check.py"); exit $LASTEXITCODE }
$cargo = Join-Path $env:USERPROFILE ".cargo\bin\cargo.exe"
if ($Crate -eq "aifam-eval") {
    # recorded answers of the family corpus; appends one ledger row. No network.
    if ($Live) {
        # needs DDC_LIVE_PROFILE, DDC_LIVE_OUT, DDC_AI_KEY; one ledger row per run
        & $cargo test -q -p ddc-app --test aifam_eval -- --ignored --nocapture live_family_tasks 2>&1 | Select-String "ok |test result|panicked|fell from|set DDC|no key" | Select-Object -First 40 | ForEach-Object { $_.Line.TrimEnd() }
        exit $LASTEXITCODE
    }
    $env:AIFAM_EVAL_LEDGER = "1"
    & $cargo test -q -p ddc-app --test aifam_eval -- --nocapture 2>&1 | Select-String "^ok |test result|panicked|FAILED|tasks not ok|fell from" | Select-Object -First 20 | ForEach-Object { $_.Line.TrimEnd() }
    exit $LASTEXITCODE
}
$pkg = if ($Crate) { @("-p", "ddc-$Crate") } else { @("--workspace") }
$fail = $false

$testArgs = @("test", "-q") + $pkg
if ($Filter) { $testArgs += $Filter }
$out = & $cargo @testArgs 2>&1 | Out-String
$out -split "`n" | Select-String "test result|panicked|^error|-->|FAILED|left:|right:" | Select-Object -First 30 | ForEach-Object { $_.Line.TrimEnd() }
if ($LASTEXITCODE -ne 0) { $fail = $true }

# AI gate: a change to crates/ai or app/src/ai replays every recorded case
$aiTouched = $Crate -eq "ai"
if ($Crate -eq "app") {
    $base = git merge-base HEAD main 2>$null
    if ($base) { $aiTouched = [bool](git diff --name-only $base -- crates/ai crates/app/src/ai) }
}
if ($aiTouched -and -not $Filter) {
    $ledger = Join-Path ([IO.Path]::GetTempPath()) "ddc_ai_eval_check.jsonl"
    $ev = & $cargo run -q --release -p ddc-app --bin ddc-ai-eval -- run --provider replay --recorded-only --ledger $ledger 2>&1 | Out-String
    $code = $LASTEXITCODE
    $ev -split "`n" | Select-String "have a recording| run, | replay_drift | fail |FAIL |^error" | Select-Object -First 15 | ForEach-Object { "ai eval: " + $_.Line.TrimEnd() }
    if ($code -eq 3) { "ai eval: no recorded case yet" }
    elseif ($code -ne 0) { "AI EVAL FAIL (replay must pass 100 %)"; $fail = $true }
}

& $cargo clippy --version *> $null
if ($LASTEXITCODE -eq 0) {
    $lint = & $cargo clippy -q @pkg 2>&1 | Out-String
    $lint -split "`n" | Select-String "^(warning|error)|-->" | Select-Object -First 20 | ForEach-Object { $_.Line.TrimEnd() }
} else { "clippy not installed (rustup component add clippy)" }

& $cargo fmt --version *> $null
if ($LASTEXITCODE -eq 0) {
    & $cargo fmt --all --check *> $null
    if ($LASTEXITCODE -ne 0) { "fmt: run cargo fmt --all" }
}
if ($fail) { "CHECK FAIL"; exit 1 } else { "CHECK OK" }
