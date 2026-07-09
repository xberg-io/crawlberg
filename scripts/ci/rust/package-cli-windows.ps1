#!/usr/bin/env pwsh

param(
    [Parameter(Mandatory=$true)]
    [string]$Target
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

Write-Host "=== Packaging CLI binary for $Target ==="

cd target/$Target/release
Compress-Archive -Path crawlberg.exe -DestinationPath ../../../crawlberg-cli-$Target.zip

Write-Host "Packaging complete: crawlberg-cli-$Target.zip"
