#!/usr/bin/env pwsh

param(
    [Parameter(Mandatory=$false)]
    [ValidateSet('fix', 'check')]
    [string]$Mode = 'check'
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$repoRoot = if ($env:REPO_ROOT) { $env:REPO_ROOT } else {
    $gitRoot = & git rev-parse --show-toplevel 2>$null
    if ($LASTEXITCODE -eq 0) { $gitRoot } else { Split-Path -Parent (Split-Path -Parent $PSScriptRoot) }
}

$goDir = Join-Path $repoRoot "packages/go"

$env:PKG_CONFIG_PATH = "$repoRoot/crates/crawlberg-ffi;$($env:PKG_CONFIG_PATH)"
if ($PSVersionTable.Platform -eq 'Win32NT' -or $PSVersionTable.PSVersion.Major -lt 6) {
    $env:PATH = "$repoRoot/target/release;$($env:PATH)"
} else {
    $env:DYLD_LIBRARY_PATH = "$repoRoot/target/debug;$($env:DYLD_LIBRARY_PATH)"
    $env:LD_LIBRARY_PATH = "$repoRoot/target/debug;$($env:LD_LIBRARY_PATH)"
}

Push-Location $goDir
try {
    switch ($Mode) {
        'fix' {
            Write-Host "Running go fmt..." -ForegroundColor Green
            & go fmt ./... 2>&1
            if ($LASTEXITCODE -ne 0) {
                Write-Host "ERROR: go fmt failed" -ForegroundColor Red
                exit 1
            }

            Write-Host "Running golangci-lint with fixes..." -ForegroundColor Green
            & golangci-lint run --config "$repoRoot/.golangci.yml" --fix ./... 2>&1
            if ($LASTEXITCODE -ne 0) {
                Write-Host "ERROR: golangci-lint fix failed" -ForegroundColor Red
                exit 1
            }

            Write-Host "Go linting with fixes completed successfully" -ForegroundColor Green
        }
        'check' {
            Write-Host "Checking Go formatting..." -ForegroundColor Green
            $unformatted = & go fmt -l ./... 2>&1
            if ($unformatted) {
                Write-Host "ERROR: Unformatted files found:" -ForegroundColor Red
                Write-Host $unformatted
                exit 1
            }

            Write-Host "Running golangci-lint check..." -ForegroundColor Green
            & golangci-lint run --config "$repoRoot/.golangci.yml" ./... 2>&1
            if ($LASTEXITCODE -ne 0) {
                Write-Host "ERROR: golangci-lint check failed" -ForegroundColor Red
                exit 1
            }

            Write-Host "Go linting check passed successfully" -ForegroundColor Green
        }
        default {
            Write-Host "ERROR: Invalid mode '$Mode'. Use 'fix' or 'check'" -ForegroundColor Red
            exit 2
        }
    }
} finally {
    Pop-Location
}
