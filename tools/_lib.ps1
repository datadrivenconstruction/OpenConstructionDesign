# SPDX-License-Identifier: AGPL-3.0-only
# SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
# Shared helpers for fuzz/corpus/bench scripts (dot-source). Windows PowerShell 5.1 and pwsh.
$script:Root = Split-Path -Parent $PSScriptRoot
$script:Cargo = Join-Path $env:USERPROFILE ".cargo\bin\cargo.exe"
$script:Utf8 = New-Object System.Text.UTF8Encoding($false)

function Get-FreeGB { [math]::Round((Get-PSDrive C).Free / 1GB, 1) }

function Assert-Disk([double]$MinGB = 5) {
    $f = Get-FreeGB
    if ($f -lt $MinGB) { Write-Error "C: free $f GB < $MinGB GB, refusing to build"; exit 2 }
}

function Get-Commit {
    $c = (git -C $script:Root rev-parse --short HEAD).Trim()
    if (git -C $script:Root status --porcelain) { $c += "-dirty" }
    $c
}

# Appends one compact JSON line (UTF-8, no BOM, LF).
function Add-Jsonl([string]$Path, $Obj) {
    $dir = Split-Path -Parent $Path
    if (-not (Test-Path $dir)) { New-Item -ItemType Directory -Force $dir | Out-Null }
    $line = ($Obj | ConvertTo-Json -Compress -Depth 6) + "`n"
    [System.IO.File]::AppendAllText($Path, $line, $script:Utf8)
}

function Read-Jsonl([string]$Path) {
    if (-not (Test-Path $Path)) { return @() }
    [System.IO.File]::ReadAllLines($Path, $script:Utf8) | Where-Object { $_.Trim() } | ForEach-Object { $_ | ConvertFrom-Json }
}

# Builds a test binary once and returns its path (CARGO_BUILD_JOBS=1 unless already set).
function Get-TestExe([string]$Pkg, [string]$Test, [string]$Profile = "release") {
    if (-not $env:CARGO_BUILD_JOBS) { $env:CARGO_BUILD_JOBS = "1" }
    Assert-Disk
    $a = @("test", "--profile", $Profile, "-p", $Pkg, "--no-run", "--message-format=json")
    if ($Test) { $a += @("--test", $Test) } else { $a += "--lib" }
    $exe = $null
    & $script:Cargo @a | ForEach-Object {
        if ($_.StartsWith("{")) {
            $m = $_ | ConvertFrom-Json
            if ($m.executable -and $m.profile.test) { $exe = $m.executable }
        }
    }
    if (-not $exe) { Write-Error "no test binary for $Pkg $Test"; exit 2 }
    $exe
}

# Runs a process, returns @{ Out; Code; Secs; PeakMB } (peak working set sampled every 25 ms).
function Invoke-Measured([string]$Exe, [string[]]$ArgList, [string]$Cwd, [hashtable]$Env = @{}) {
    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $psi.FileName = $Exe
    $psi.Arguments = ($ArgList | ForEach-Object { if ($_ -match '\s') { '"' + $_ + '"' } else { $_ } }) -join " "
    $psi.WorkingDirectory = $Cwd
    $psi.UseShellExecute = $false
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError = $true
    foreach ($k in $Env.Keys) { $psi.EnvironmentVariables[$k] = [string]$Env[$k] }
    $sw = [Diagnostics.Stopwatch]::StartNew()
    $p = [Diagnostics.Process]::Start($psi)
    $so = $p.StandardOutput.ReadToEndAsync()
    $se = $p.StandardError.ReadToEndAsync()
    $peak = 0L
    while (-not $p.HasExited) {
        try { $p.Refresh(); if ($p.PeakWorkingSet64 -gt $peak) { $peak = $p.PeakWorkingSet64 } } catch {}
        Start-Sleep -Milliseconds 25
    }
    $p.WaitForExit()
    $sw.Stop()
    @{ Out = $so.Result + "`n" + $se.Result; Code = $p.ExitCode; Secs = $sw.Elapsed.TotalSeconds; PeakMB = [math]::Round($peak / 1MB, 1) }
}
