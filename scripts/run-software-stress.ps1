[CmdletBinding()]
param([switch]$SkipBuild)
$ErrorActionPreference = 'Stop'
$projectRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
Push-Location $projectRoot
try {
    if (-not $SkipBuild) {
        & cargo build -p pcanwork
        if ($LASTEXITCODE -ne 0) { throw 'Debug build failed.' }
    }
    $reportDir = Join-Path $projectRoot ('artifacts\software-stress\' + (Get-Date -Format 'yyyyMMdd-HHmmss-fff'))
    New-Item -ItemType Directory -Path $reportDir | Out-Null
    $executable = Join-Path $projectRoot 'target\debug\pcanwork.exe'
    $argument = '"--software-stress=' + $reportDir + '"'
    $testProcess = Start-Process -FilePath $executable -ArgumentList $argument -WorkingDirectory $projectRoot -WindowStyle Hidden -PassThru -RedirectStandardOutput (Join-Path $reportDir 'stdout.log') -RedirectStandardError (Join-Path $reportDir 'stderr.log')
    if (-not $testProcess.WaitForExit(180000)) {
        Stop-Process -Id $testProcess.Id
        throw "Software stress timeout; see $reportDir"
    }
    $report = Join-Path $reportDir 'report.json'
    if ($testProcess.ExitCode -ne 0 -or -not (Test-Path -LiteralPath $report)) {
        Get-Content -LiteralPath (Join-Path $reportDir 'stderr.log')
        throw "Software stress failed; see $reportDir"
    }
    Get-Content -LiteralPath $report
    Write-Output "Report: $report"
} finally {
    Pop-Location
}
