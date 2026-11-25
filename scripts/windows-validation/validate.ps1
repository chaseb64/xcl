param([Parameter(Mandatory)][ValidateSet('baseline', 'candidate')][string]$Role)

$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false
$pair = Get-Content -Raw -LiteralPath (Join-Path $PSScriptRoot 'revisions.json') | ConvertFrom-Json
$revision = $pair.$Role
$source = Join-Path $env:RUNNER_TEMP "xcl-$Role"
$logDir = (New-Item -Force -ItemType Directory -Path (Join-Path $env:EVIDENCE $Role)).FullName
$toolchain = '+stable-x86_64-pc-windows-msvc'
$compute = 'gpu::recomp::mesh_dispatch::tests::gpu_dispatch_matches_native_wave64_reference'
$dxc = 'gpu::recomp::dxc::version_tests::the_loaded_dxc_is_identified'
$title = 'test_title_runs_and_game_folder_is_untouched'
$report = [ordered]@{ role = $Role; revision = $revision; adapter = $env:XCL_ADAPTER; checks = [ordered]@{} }

function Save-Report {
    $report | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath (Join-Path $logDir 'results.json')
}

function Invoke-Check([string]$Name, [string]$Program, [string[]]$Arguments) {
    $log = Join-Path $logDir "$Name.log"
    "> $Program $($Arguments -join ' ')" | Tee-Object -FilePath $log | Out-Host
    & $Program @Arguments 2>&1 | ForEach-Object { $_.ToString() } |
        Tee-Object -FilePath $log -Append | Out-Host
    $code = $LASTEXITCODE
    "exit_code=$code" | Tee-Object -FilePath $log -Append | Out-Host
    $report.checks[$Name] = [ordered]@{ exit_code = $code; log = "$Name.log" }
    Save-Report
    return $code
}

function Measure-Suite([string]$Name) {
    $log = Get-Content -Raw -LiteralPath (Join-Path $logDir "$Name.log")
    $totals = [ordered]@{ summaries = 0; passed = 0; failed = 0; ignored = 0; measured = 0; filtered = 0 }
    $pattern = '(?m)^test result: (?:ok|FAILED)\. (\d+) passed; (\d+) failed; (\d+) ignored; (\d+) measured; (\d+) filtered out'
    foreach ($summary in [regex]::Matches($log, $pattern)) {
        $totals.summaries++
        $totals.passed += [int]$summary.Groups[1].Value
        $totals.failed += [int]$summary.Groups[2].Value
        $totals.ignored += [int]$summary.Groups[3].Value
        $totals.measured += [int]$summary.Groups[4].Value
        $totals.filtered += [int]$summary.Groups[5].Value
    }
    $skips = @([regex]::Matches($log, 'SKIPPED [^\s]+: [^\r\n]+') | ForEach-Object { $_.Value })
    $totals['gpu_skip_events'] = $skips.Count
    $totals['gpu_skip_names'] = @($skips | ForEach-Object { ($_ -split ': ', 2)[0] } | Sort-Object -Unique).Count
    Set-Content -LiteralPath (Join-Path $logDir "$Name-skips.txt") -Value ($skips -join "`n")
    $report.checks[$Name]['tests'] = $totals
    Save-Report
    return $totals
}

Save-Report
if ((Invoke-Check 'checkout' git @('-c', 'core.autocrlf=false', 'worktree', 'add', '--detach', $source, $revision)) -ne 0) {
    throw "Could not check out $revision."
}
Push-Location -LiteralPath $source
try {
    $actual = git rev-parse HEAD
    if ($LASTEXITCODE -ne 0 -or $actual -cne $revision) { throw "Unexpected source revision: $actual" }
    $report['source_tree'] = git rev-parse 'HEAD^{tree}'
    $report['workspace_exclusion'] = $title
    Save-Report

    $build = Invoke-Check 'workspace-build' cargo @($toolchain, 'build', '--workspace', '--locked')
    $workspace = Invoke-Check 'workspace-test' cargo @(
        $toolchain, 'test', '--workspace', '--locked', '--no-fail-fast', '--',
        '--test-threads=4', '--show-output', '--skip', $title
    )
    $workspaceCounts = Measure-Suite 'workspace-test'

    $readback = Invoke-Check 'warp-compute' cargo @(
        $toolchain, 'test', '--locked', '-p', 'xcl', '--lib', $compute, '--', '--exact', '--nocapture'
    )
    $readbackCounts = Measure-Suite 'warp-compute'
    $readbackRan = $readback -eq 0 -and $readbackCounts.passed -eq 1 -and $readbackCounts.gpu_skip_events -eq 0
    $report.checks['warp-compute']['readback_assertions_passed'] = $readbackRan
    Save-Report

    $dxcExit = Invoke-Check 'dxc' cargo @(
        $toolchain, 'test', '--locked', '-p', 'xcl', '--lib', $dxc, '--', '--exact', '--nocapture'
    )
    $dxcCounts = Measure-Suite 'dxc'
    $dxcLine = Select-String -LiteralPath (Join-Path $logDir 'dxc.log') -Pattern '^DXC: '
    $report.checks['dxc']['loaded'] = $dxcLine.Line
    Save-Report

    if ($build -ne 0 -or $workspace -ne 0 -or $workspaceCounts.summaries -eq 0 -or $workspaceCounts.failed -ne 0 -or
        -not $readbackRan -or $dxcExit -ne 0 -or $dxcCounts.passed -ne 1 -or $dxcCounts.gpu_skip_events -ne 0 -or
        -not $dxcLine) {
        throw "$Role validation failed. See results.json and the per-check logs."
    }

    "| $Role | $revision | $($workspaceCounts.passed) | $($workspaceCounts.failed) | $($workspaceCounts.ignored) | $($workspaceCounts.gpu_skip_events) | WARP readback passed |" |
        Add-Content -LiteralPath (Join-Path $logDir 'summary.md')
    @(
        "### $Role"
        ''
        '| Revision role | SHA | Passed | Failed | Ignored | GPU skip events | Synthetic compute |'
        '| --- | --- | ---: | ---: | ---: | ---: | --- |'
        (Get-Content -LiteralPath (Join-Path $logDir 'summary.md'))
        ''
        $dxcLine.Line
    ) | Add-Content -LiteralPath $env:GITHUB_STEP_SUMMARY
} finally {
    Pop-Location
}
