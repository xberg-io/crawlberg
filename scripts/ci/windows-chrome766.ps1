param(
    [ValidateSet('Build', 'Probe', 'SelfTest')][string]$Mode,
    [string]$OutputDirectory = $env:CRAWLBERG_DIAGNOSTICS_DIR
)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false
if ([string]::IsNullOrWhiteSpace($OutputDirectory)) { throw 'Diagnostic output directory must be explicit' }
[void](New-Item -ItemType Directory -Force $OutputDirectory)

function Invoke-RecordedProcess([string]$Program, [string[]]$Arguments, [string]$Name, [int]$TimeoutSeconds = 90) {
    $info = [System.Diagnostics.ProcessStartInfo]::new($Program)
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

function Build-TestExecutables {
    $commands = @(
        @('test', '--locked', '--manifest-path', 'vendor/crawlberg-chromiumoxide/Cargo.toml', '--lib'),
        @('test', '-p', 'crawlberg', '--features', 'browser,interact', '--lib'),
        @('test', '-p', 'crawlberg', '--features', 'browser,interact',
            '--test', 'test_browser_profile_cleanup', '--test', 'test_browser_pool_lifecycle')
    )
    $wanted = @('chromiumoxide', 'crawlberg', 'test_browser_pool_lifecycle', 'test_browser_profile_cleanup')
    $artifacts = @()
    for ($index = 0; $index -lt $commands.Count; $index++) {
        $arguments = $commands[$index] + @('--no-run', '--message-format=json')
        $cargo = (Get-Command cargo -CommandType Application).Source
        $result = Invoke-RecordedProcess $cargo $arguments "build-$index" 1200
        if ($result.exit_code -ne 0 -or $result.timed_out) {
            throw "Build $index failed; see preserved build logs"
        }
        foreach ($line in $result.stdout -split "`n") {
            if (-not $line.StartsWith('{')) { continue }
            $message = $line | ConvertFrom-Json -Depth 100
            if ($message.reason -eq 'compiler-artifact' -and $message.executable -and
                $message.profile.test -and $message.target.name -in $wanted) {
                $artifacts += [pscustomobject]@{
                    name = $message.target.name; path = $message.executable
                    sha256 = (Get-FileHash $message.executable -Algorithm SHA256).Hash
                }
            }
        }
    }
    $names = @($artifacts.name | Sort-Object) -join ','
    if ($artifacts.Count -ne 4 -or $names -ne (@($wanted | Sort-Object) -join ',')) {
        throw 'Expected exactly four existing test executables'
    }
    Save-Json $artifacts 'executables.json'
    Write-Output 'Expected executables: 4; observed executables: 4'
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

function Get-TestSelection([string]$Name, [bool]$Suite) {
    $expected = switch ($Name) {
        'chromiumoxide' { 47 }
        'crawlberg' { 2 }
        'test_browser_pool_lifecycle' { if ($Suite) { 3 } else { 1 } }
        'test_browser_profile_cleanup' { 1 }
    }
    $arguments = @('--nocapture', '--test-threads=1')
    if ($Name -eq 'crawlberg') {
        $arguments += @('--exact',
            'browser_pool::tests::a_process_started_at_once_by_the_launched_one_is_in_its_process_tree',
            'browser_pool::tests::a_launched_profile_holds_the_process_tree_of_its_chrome')
    } elseif (-not $Suite) {
        $arguments += @('--exact', 'pool_bounds_concurrent_navigations_to_max_pages')
    }
    return [pscustomobject]@{ expected = $expected; arguments = $arguments }
}

function Invoke-ProbeCase($Binary, $Chrome, [bool]$Suite, [string]$Name, [DateTime]$Deadline) {
    $selection = Get-TestSelection $Binary.name $Suite
    $row = [ordered]@{
        browser = $Chrome.version; binary = $Binary.name
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
        $result = Invoke-RecordedProcess $Binary.path $selection.arguments $Name $timeout
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
        expected_rows = 18; observed_rows = $Rows.Count
        executed_rows = @($Rows | Where-Object executed).Count
        expected_suite_tests_per_version = 53; expected_replays_per_version = 5
        expected_total_test_executions = 116; rows = $Rows
        interpretation = 'Version differences are provisional; bytes and five counterbalanced replays are controlled.'
    }
    Save-Json $receipt 'results.json'
}

function Run-ChromeProbe {
    $binaries = @(Get-Content -Raw (Join-Path $OutputDirectory 'executables.json') | ConvertFrom-Json)
    if ($binaries.Count -ne 4) { throw 'Expected four compiled executable receipts' }
    $browsers = @((Get-Chrome $env:CHROME_155_PATH '155.0.8059.84'),
        (Get-Chrome $env:CHROME_156_PATH '156.0.8078.47'))
    if ($browsers[0].path -eq $browsers[1].path -or $browsers[0].sha256 -eq $browsers[1].sha256) {
        throw 'The two Chrome installations must be distinct'
    }
    Save-Json $browsers 'browsers.json'
    $sequence = @(@{ browser = 0; suite = $true }, @{ browser = 1; suite = $true })
    for ($block = 0; $block -lt 5; $block++) {
        foreach ($browser in @((($block % 2)), (1 - ($block % 2)))) {
            $sequence += @{ browser = $browser; suite = $false }
        }
    }
    $rows = [System.Collections.Generic.List[object]]::new()
    $deadline = [DateTime]::UtcNow.AddMinutes(9)
    foreach ($entry in $sequence) {
        $chrome = $browsers[$entry.browser]
        $env:CHROME = $chrome.path
        $env:CRAWLBERG_TEST_CHROME_PATH = $chrome.path
        $selected = if ($entry.suite) { $binaries } else {
            @($binaries | Where-Object name -eq 'test_browser_pool_lifecycle')
        }
        foreach ($binary in $selected) {
            $name = '{0:D2}-{1}-{2}' -f $rows.Count, $chrome.version, $binary.name
            $row = Invoke-ProbeCase $binary $chrome $entry.suite $name $deadline
            $rows.Add($row)
            Save-ProbeRows $rows
            Write-Output "$name expected=$($row.expected) ok=$($row.ok)"
        }
    }
    foreach ($binary in $binaries) {
        if ((Get-FileHash $binary.path -Algorithm SHA256).Hash -ne $binary.sha256) {
            throw 'Final executable bytes changed'
        }
    }
    if ($rows.Count -ne 18 -or @($rows | Where-Object { -not $_.ok }).Count -ne 0) {
        throw 'Probe failed; inspect all eighteen expected rows and preserved logs'
    }
    Write-Output 'Expected rows: 18; observed rows: 18; all passing'
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
