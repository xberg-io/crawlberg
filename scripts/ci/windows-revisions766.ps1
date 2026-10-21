param(
    [ValidateSet('Build', 'Probe', 'SelfTest')][string]$Mode,
    [string]$OutputDirectory = $env:CRAWLBERG_DIAGNOSTICS_DIR
)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false
if ([string]::IsNullOrWhiteSpace($OutputDirectory)) { throw 'Diagnostic output directory must be explicit' }
[void](New-Item -ItemType Directory -Force $OutputDirectory)

function Invoke-RecordedProcess([string]$Program, [string[]]$Arguments, [string]$Name, [int]$TimeoutSeconds = 90,
    [string]$WorkingDirectory = (Get-Location).Path) {
    $info = [System.Diagnostics.ProcessStartInfo]::new($Program)
    $info.WorkingDirectory = $WorkingDirectory
    $info.UseShellExecute = $false
    $info.RedirectStandardOutput = $true
    $info.RedirectStandardError = $true
    foreach ($argument in $Arguments) { $info.ArgumentList.Add($argument) }
    $process = [System.Diagnostics.Process]::new()
    $process.StartInfo = $info
    [void]$process.Start()
    $stdout = $process.StandardOutput.ReadToEndAsync()
    $stderr = $process.StandardError.ReadToEndAsync()
    $timedOut = -not $process.WaitForExit($TimeoutSeconds * 1000)
    if ($timedOut) { $process.Kill($true); $process.WaitForExit() }
    $result = [pscustomobject]@{
        exit_code = $process.ExitCode; timed_out = $timedOut
        stdout = $stdout.GetAwaiter().GetResult(); stderr = $stderr.GetAwaiter().GetResult()
    }
    $result.stdout | Set-Content -Encoding utf8NoBOM (Join-Path $OutputDirectory "$Name.stdout.log")
    $result.stderr | Set-Content -Encoding utf8NoBOM (Join-Path $OutputDirectory "$Name.stderr.log")
    $process.Dispose()
    return $result
}

function Assert-TestResult($Result, [int]$Expected) {
    $pattern = 'test result: (\w+)\. (\d+) passed; (\d+) failed; (\d+) ignored;'
    $matches = [regex]::Matches($Result.stdout, $pattern)
    if ($Result.exit_code -ne 0 -or $Result.timed_out -or $matches.Count -ne 1) {
        throw 'Native test execution failed'
    }
    $summary = $matches[0].Groups
    if ($summary[1].Value -ne 'ok' -or [int]$summary[2].Value -ne $Expected -or
        [int]$summary[3].Value -ne 0 -or [int]$summary[4].Value -ne 0) {
        throw "Expected $Expected actual passing tests"
    }
    if (($Result.stdout + "`n" + $Result.stderr) -match '(?im)^skipping .*Chrome') {
        throw 'Chrome fixture skipped'
    }
}

function Save-Json($Value, [string]$Name) {
    ConvertTo-Json -InputObject $Value -Depth 15 |
        Set-Content -Encoding utf8NoBOM (Join-Path $OutputDirectory $Name)
}

function Get-RevisionSource([string]$Revision, [string]$Name) {
    $directory = Join-Path $env:RUNNER_TEMP "crawlberg-766-$Name"
    if (Test-Path $directory) { throw 'Revision source directory must be fresh' }
    [void](New-Item -ItemType Directory $directory)
    $archive = Join-Path $OutputDirectory "$Name-source.tar"
    $git = (Get-Command git -CommandType Application | Select-Object -First 1).Source
    $result = Invoke-RecordedProcess $git @('archive', '--format=tar', "--output=$archive", $Revision) "archive-$Name"
    if ($result.exit_code -ne 0 -or $result.timed_out) { throw "Unable to archive $Revision" }
    $tar = (Get-Command tar -CommandType Application | Select-Object -First 1).Source
    $result = Invoke-RecordedProcess $tar @('-xf', $archive, '-C', $directory) "extract-$Name"
    if ($result.exit_code -ne 0 -or $result.timed_out) { throw "Unable to extract $Revision" }
    return [pscustomobject]@{
        name = $Name; revision = $Revision; directory = $directory
        archive_sha256 = (Get-FileHash $archive -Algorithm SHA256).Hash
    }
}

function Build-Revision($Source, [string]$Fixture) {
    $fixturePath = Join-Path $Source.directory 'crates/crawlberg/tests/test_browser_pool_lifecycle.rs'
    $originalFixtureHash = (Get-FileHash $fixturePath -Algorithm SHA256).Hash
    if ($fixturePath -ne $Fixture) { Copy-Item -LiteralPath $Fixture -Destination $fixturePath }
    $arguments = @('+1.99.1', 'test', '--locked', '-p', 'crawlberg', '--features', 'browser,interact',
        '--test', 'test_browser_pool_lifecycle', '--no-run', '--message-format=json')
    $lock = Join-Path $Source.directory 'Cargo.lock'
    $lockHash = (Get-FileHash $lock -Algorithm SHA256).Hash
    $cargo = (Get-Command cargo -CommandType Application | Select-Object -First 1).Source
    $result = Invoke-RecordedProcess $cargo $arguments "build-$($Source.name)" 1200 $Source.directory
    if ($result.exit_code -ne 0 -or $result.timed_out) { throw 'Revision build failed; full logs preserved' }
    if ((Get-FileHash $lock -Algorithm SHA256).Hash -ne $lockHash) { throw 'Revision lockfile changed' }
    $artifacts = @()
    foreach ($line in $result.stdout -split "`n") {
        if (-not $line.StartsWith('{')) { continue }
        $message = $line | ConvertFrom-Json -Depth 100
        if ($message.reason -eq 'compiler-artifact' -and $message.executable -and
            $message.profile.test -and $message.target.name -eq 'test_browser_pool_lifecycle') {
            $preserved = Join-Path $OutputDirectory "$($Source.name)-lifecycle.exe"
            Copy-Item -LiteralPath $message.executable -Destination $preserved
            $artifacts += [pscustomobject]@{
                name = $message.target.name; revision = $Source.revision; label = $Source.name; path = $preserved
                cwd = Split-Path $message.manifest_path; sha256 = (Get-FileHash $preserved -Algorithm SHA256).Hash
                fixture_sha256 = (Get-FileHash $fixturePath -Algorithm SHA256).Hash
                original_fixture_sha256 = $originalFixtureHash; archive_sha256 = $Source.archive_sha256
                lock_sha256 = $lockHash; toolchain = '1.99.1'
            }
        }
    }
    if ($artifacts.Count -ne 1) { throw 'Expected exactly one lifecycle executable per revision' }
    return $artifacts[0]
}

function Build-TestExecutables {
    $current = Get-RevisionSource '51645bb2ee16a40a1e0ca345c5294d5010fdbf5d' 'current'
    $old = Get-RevisionSource '3a8e6fa6c4906f0bc65557e02aac221eef3cf48b' 'old'
    Save-Json @($old, $current) 'sources.json'
    $rustc = (Get-Command rustc -CommandType Application | Select-Object -First 1).Source
    $version = Invoke-RecordedProcess $rustc @('+1.99.1', '--version', '--verbose') 'rustc-version'
    if ($version.exit_code -ne 0 -or $version.timed_out) { throw 'Pinned Rust1.99.1 is unavailable' }
    $fixture = Join-Path $current.directory 'crates/crawlberg/tests/test_browser_pool_lifecycle.rs'
    $env:CARGO_TARGET_DIR = Join-Path (Get-Location).Path 'target'
    $currentBinary = Build-Revision $current $fixture
    Save-Json @($currentBinary) 'executables.json'
    $oldBinary = Build-Revision $old $fixture
    if ($oldBinary.fixture_sha256 -ne $currentBinary.fixture_sha256) { throw 'Fixture bytes differ' }
    Save-Json @($oldBinary, $currentBinary) 'executables.json'
    Write-Output 'Expected executables: 2; observed executables: 2; identical current fixture'
}

function Get-Chrome([string]$Path, [string]$Version) {
    if ([string]::IsNullOrWhiteSpace($Path) -or -not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        throw "Chrome $Version is missing"
    }
    $file = Get-Item -LiteralPath $Path
    $pattern = '^(\d+\.\d+\.\d+\.\d+)(?:\s|$)'
    $numericVersion = [regex]::Match($file.VersionInfo.ProductVersion, $pattern).Groups[1].Value
    if ($numericVersion -ne $Version) { throw "Expected Chrome $Version; found $($file.VersionInfo.ProductVersion)" }
    return [pscustomobject]@{
        version = $Version; product_version = $file.VersionInfo.ProductVersion; path = $file.FullName
        sha256 = (Get-FileHash $file.FullName -Algorithm SHA256).Hash
    }
}

function Get-TestSelection([bool]$Suite) {
    $expected = if ($Suite) { 3 } else { 1 }
    $arguments = @('--nocapture', '--test-threads=1')
    if (-not $Suite) {
        $arguments += @('--exact', 'pool_bounds_concurrent_navigations_to_max_pages')
    }
    return [pscustomobject]@{ expected = $expected; arguments = $arguments }
}

function Invoke-ProbeCase($Binary, $Chrome, [bool]$Suite, [string]$Name, [DateTime]$Deadline) {
    $selection = Get-TestSelection $Suite
    $row = [ordered]@{
        browser = $Chrome.version; binary = $Binary.name; revision = $Binary.revision; label = $Binary.label
        phase = $(if ($Suite) { 'suite' } else { 'replay' })
        expected = $selection.expected; sha256 = $Binary.sha256
        ok = $false; executed = $false; log_prefix = $Name
    }
    try {
        if ((Get-FileHash $Binary.path -Algorithm SHA256).Hash -ne $Binary.sha256) {
            throw 'Test executable bytes changed'
        }
        $remaining = [int][Math]::Floor(($Deadline - [DateTime]::UtcNow).TotalSeconds)
        if ($remaining -le 0) { throw 'Nine-minute probe budget exhausted; case not executed' }
        $timeout = [Math]::Min(90, $remaining)
        $result = Invoke-RecordedProcess $Binary.path $selection.arguments $Name $timeout $Binary.cwd
        $row.executed = $true
        $row.native_exit_code = $result.exit_code
        $row.timed_out = $result.timed_out
        $row.summaries = @([regex]::Matches($result.stdout, 'test result: [^\r\n]+') | ForEach-Object Value)
        Assert-TestResult $result $selection.expected
        $row.ok = $true
    } catch { $row.error = $_.Exception.Message }
    return [pscustomobject]$row
}

function Save-ProbeRows($Rows) {
    $receipt = @{
        expected_rows = 22; observed_rows = $Rows.Count
        executed_rows = @($Rows | Where-Object executed).Count
        expected_suite_tests_per_revision = 3; expected_replays_per_revision = 10
        expected_total_test_executions = 26; rows = $Rows
        interpretation = 'Identical current fixture, Chrome155, two production revisions; no commit attribution.'
    }
    Save-Json $receipt 'results.json'
}

function Run-ChromeProbe {
    $binaries = @(Get-Content -Raw (Join-Path $OutputDirectory 'executables.json') | ConvertFrom-Json)
    if ($binaries.Count -ne 2 -or $binaries[0].label -ne 'old' -or $binaries[1].label -ne 'current') {
        throw 'Expected old and current compiled lifecycle executable receipts'
    }
    if ($binaries[0].fixture_sha256 -ne $binaries[1].fixture_sha256) { throw 'Fixture bytes differ' }
    $chrome = Get-Chrome $env:CHROME_155_PATH '155.0.8059.84'
    Save-Json @($chrome) 'browsers.json'
    $env:CHROME = $chrome.path
    $env:CRAWLBERG_TEST_CHROME_PATH = $chrome.path
    $sequence = @(@{ binary = 0; suite = $true }, @{ binary = 1; suite = $true })
    for ($block = 0; $block -lt 10; $block++) {
        foreach ($binary in @((($block % 2)), (1 - ($block % 2)))) {
            $sequence += @{ binary = $binary; suite = $false }
        }
    }
    $rows = [System.Collections.Generic.List[object]]::new()
    $deadline = [DateTime]::UtcNow.AddMinutes(9)
    foreach ($entry in $sequence) {
        $binary = $binaries[$entry.binary]
        $name = '{0:D2}-{1}-{2}' -f $rows.Count, $binary.label, $binary.name
        $row = Invoke-ProbeCase $binary $chrome $entry.suite $name $deadline
        $rows.Add($row)
        Save-ProbeRows $rows
        Write-Output "$name expected=$($row.expected) ok=$($row.ok)"
    }
    foreach ($binary in $binaries) {
        if ((Get-FileHash $binary.path -Algorithm SHA256).Hash -ne $binary.sha256) {
            throw 'Final executable bytes changed'
        }
    }
    if ($rows.Count -ne 22 -or @($rows | Where-Object { -not $_.ok }).Count -ne 0) {
        throw 'Probe failed; inspect all twenty-two expected rows and preserved logs'
    }
    Write-Output 'Expected rows: 22; observed rows: 22; all passing'
}

function Test-Harness {
    $positive = [pscustomobject]@{
        exit_code = 0; timed_out = $false
        stdout = 'test result: ok. 3 passed; 0 failed; 0 ignored;'; stderr = ''
    }
    Assert-TestResult $positive 3
    $negative = @(
        @{ stdout = 'test result: ok. 0 passed; 0 failed; 0 ignored;'
            stderr = ''; exit_code = 0; timed_out = $false },
        @{ stdout = 'test result: FAILED. 2 passed; 1 failed; 0 ignored;'
            stderr = ''; exit_code = 101; timed_out = $false },
        @{ stdout = 'test result: ok. 3 passed; 0 failed; 1 ignored;'
            stderr = ''; exit_code = 0; timed_out = $false },
        @{ stdout = $positive.stdout; stderr = 'skipping fixture because no usable Chrome was found'
            exit_code = 0; timed_out = $false }
    )
    $rejected = 0
    foreach ($case in $negative) {
        try { Assert-TestResult ([pscustomobject]$case) 3 } catch {
            $rejected++
            Write-Output ('expected rejection: ' + $_.Exception.Message)
        }
    }
    if ($rejected -ne 4) { throw "Expected four negative controls to fail; observed $rejected" }
    $command = "Write-Output 'test result: ok. 3 passed; 0 failed; 0 ignored;'"
    $arguments = @('-NoProfile', '-Command', $command)
    $child = Invoke-RecordedProcess (Get-Process -Id $PID).Path $arguments 'self-test-child' 10
    Assert-TestResult $child 3
    Write-Output 'Harness controls: 2 positive passes, 4 expected failures'
}

switch ($Mode) {
    'Build' { Build-TestExecutables }
    'Probe' { Run-ChromeProbe }
    'SelfTest' { Test-Harness }
}
