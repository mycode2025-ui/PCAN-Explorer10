[CmdletBinding(SupportsShouldProcess)]
param([ValidateRange(1,365)][int]$KeepDays = 7, [switch]$Apply)
$ErrorActionPreference = 'Stop'
$root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
if (Get-Process cargo,rustc -ErrorAction SilentlyContinue) {
    throw 'Cargo or rustc is running. Retry after the build finishes.'
}
$cutoff = (Get-Date).AddDays(-$KeepDays)
$targets = [Collections.Generic.List[string]]::new()
foreach ($relative in @('artifacts\target-release-no-lto', 'vendor\slint\i-slint-core\target')) {
    $candidate = Join-Path $root $relative
    if (Test-Path -LiteralPath $candidate) { $targets.Add($candidate) }
}
# Keep recent incremental units and all dependency, build-script and release caches.
foreach ($profile in @('debug','release-fast')) {
    $incremental = Join-Path $root "target\$profile\incremental"
    if (Test-Path -LiteralPath $incremental) {
        $keep = @(Get-ChildItem -LiteralPath $incremental -Directory |
            Group-Object { $_.Name -replace '-[^-]+$','' } | ForEach-Object {
                $_.Group | Sort-Object LastWriteTime -Descending | Select-Object -First 1 -ExpandProperty FullName
            })
        foreach ($dir in Get-ChildItem -LiteralPath $incremental -Directory) {
            if ($keep -contains $dir.FullName) { continue }
            $latest = Get-ChildItem -LiteralPath $dir.FullName -Recurse -File |
                Sort-Object LastWriteTime -Descending | Select-Object -First 1
            if ($dir.LastWriteTime -lt $cutoff -and (!$latest -or $latest.LastWriteTime -lt $cutoff)) {
                $targets.Add($dir.FullName)
            }
        }
    }
}
$rows = foreach ($target in $targets) {
    $resolved = (Resolve-Path -LiteralPath $target).Path
    if (!$resolved.StartsWith($root + '\', [StringComparison]::OrdinalIgnoreCase)) { throw "Outside workspace: $resolved" }
    $items = @(Get-Item -LiteralPath $resolved) + @(Get-ChildItem -LiteralPath $resolved -Recurse -Force)
    if ($items | Where-Object { $_.Attributes -band [IO.FileAttributes]::ReparsePoint }) { throw "Reparse point in cache: $resolved" }
    $bytes = ($items | Where-Object { !$_.PSIsContainer } | Measure-Object Length -Sum).Sum
    $removed = $false
    if ($Apply -and $PSCmdlet.ShouldProcess($resolved, 'Delete regenerable build cache')) {
        Remove-Item -LiteralPath $resolved -Recurse -Force
        $removed = $true
    }
    [pscustomobject]@{Path=$resolved; Bytes=$bytes; Removed=$removed}
}
$rows
Write-Host ('Cache candidates: {0:N2} GiB; apply={1}' -f (($rows | Measure-Object Bytes -Sum).Sum / 1GB), $Apply)
