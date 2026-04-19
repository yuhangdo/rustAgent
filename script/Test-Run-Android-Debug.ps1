param()

$ErrorActionPreference = "Stop"

$repoRoot = Split-Path -Parent $PSScriptRoot
$scriptPath = Join-Path $PSScriptRoot "run-android-debug.ps1"

function Assert-Contains {
    param(
        [string]$Text,
        [string]$Expected,
        [string]$Label
    )

    if ($Text -notmatch [regex]::Escape($Expected)) {
        throw "Expected dry-run output to contain [$Expected] for $Label."
    }
}

if (-not (Test-Path $scriptPath)) {
    throw "Expected main script to exist at $scriptPath"
}

$output = & powershell -ExecutionPolicy Bypass -File $scriptPath -DryRun -AvdName "rustAgent_API_34" 2>&1 | Out-String

Assert-Contains -Text $output -Expected "Select emulator target" -Label "target selection"
Assert-Contains -Text $output -Expected "Start emulator if needed" -Label "emulator boot"
Assert-Contains -Text $output -Expected ".\gradlew.bat testDebugUnitTest" -Label "unit tests"
Assert-Contains -Text $output -Expected ".\gradlew.bat assembleDebug" -Label "debug build"
Assert-Contains -Text $output -Expected "adb -s <serial> install -r" -Label "apk install"
Assert-Contains -Text $output -Expected "adb -s <serial> shell am start -W" -Label "app launch"

Write-Host "run-android-debug dry-run test passed."
