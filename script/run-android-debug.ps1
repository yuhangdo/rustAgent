param(
    [string]$AvdName,
    [string]$PackageName = "com.yuhangdo.rustagent",
    [string]$GradleTaskForTests = "testDebugUnitTest",
    [string]$GradleTaskForBuild = "assembleDebug",
    [string]$ApkPath = "app/build/outputs/apk/debug/app-debug.apk",
    [switch]$SkipTests,
    [switch]$SkipBuild,
    [switch]$DryRun
)

$ErrorActionPreference = "Stop"

$repoRoot = Split-Path -Parent $PSScriptRoot
$defaultAvdName = "rustAgent_API_34"

function Write-Step {
    param([string]$Message)
    Write-Host "==> $Message"
}

function Read-AndroidSdkDirFromLocalProperties {
    $localPropertiesPath = Join-Path $repoRoot "local.properties"
    if (-not (Test-Path $localPropertiesPath)) {
        return $null
    }

    $sdkLine = Get-Content $localPropertiesPath |
        Where-Object { $_ -match '^sdk\.dir=' } |
        Select-Object -First 1

    if (-not $sdkLine) {
        return $null
    }

    $rawValue = $sdkLine.Substring("sdk.dir=".Length).Trim()
    return $rawValue.Replace('\:', ':').Replace('\\', '\')
}

function Resolve-AndroidSdkDir {
    $candidates = @(
        $env:ANDROID_SDK_ROOT,
        $env:ANDROID_HOME,
        (Read-AndroidSdkDirFromLocalProperties)
    ) | Where-Object { $_ }

    foreach ($candidate in $candidates) {
        if (Test-Path $candidate) {
            return (Resolve-Path $candidate).Path
        }
    }

    throw "Android SDK was not found. Set ANDROID_SDK_ROOT/ANDROID_HOME or configure sdk.dir in local.properties."
}

function Resolve-ToolPath {
    param(
        [string]$CommandName,
        [string[]]$Candidates = @()
    )

    $resolved = Get-Command $CommandName -ErrorAction SilentlyContinue
    if ($resolved) {
        return $resolved.Source
    }

    foreach ($candidate in $Candidates) {
        if ($candidate -and (Test-Path $candidate)) {
            return (Resolve-Path $candidate).Path
        }
    }

    throw "Required command was not found: $CommandName"
}

function Get-AvailableAvdNames {
    param([string]$EmulatorPath)

    $avds = & $EmulatorPath -list-avds
    return @($avds | Where-Object { $_ -and $_.Trim() })
}

function Get-RunningEmulators {
    param([string]$AdbPath)

    $serials = @()
    $deviceLines = & $AdbPath devices -l
    foreach ($line in $deviceLines) {
        if ($line -match '^(emulator-\d+)\s+device\b') {
            $serials += $Matches[1]
        }
    }

    $result = @()
    foreach ($serial in $serials) {
        $avdName = ""
        try {
            $rawAvdName = & $AdbPath -s $serial shell getprop ro.boot.qemu.avd_name 2>$null
            if ($null -ne $rawAvdName) {
                $avdName = ($rawAvdName | Out-String).Trim()
            }
        } catch {
            $avdName = ""
        }

        $result += [pscustomobject]@{
            Serial = $serial
            AvdName = $avdName
        }
    }

    return $result
}

function Resolve-TargetAvdName {
    param(
        [string]$RequestedAvdName,
        [string[]]$AvailableAvds
    )

    if ($RequestedAvdName) {
        if ($AvailableAvds -notcontains $RequestedAvdName) {
            throw "Requested AVD '$RequestedAvdName' was not found. Available AVDs: $($AvailableAvds -join ', ')"
        }
        return $RequestedAvdName
    }

    if ($AvailableAvds -contains $defaultAvdName) {
        return $defaultAvdName
    }

    if ($AvailableAvds.Count -gt 0) {
        return $AvailableAvds[0]
    }

    throw "No Android Virtual Device (AVD) was found."
}

function Wait-ForNewEmulator {
    param(
        [string]$AdbPath,
        [string]$ExpectedAvdName,
        [string[]]$ExistingSerials,
        [int]$TimeoutSeconds = 180
    )

    $deadline = (Get-Date).AddSeconds($TimeoutSeconds)
    do {
        Start-Sleep -Seconds 5
        $running = Get-RunningEmulators -AdbPath $AdbPath
        foreach ($emulator in $running) {
            if ($ExistingSerials -contains $emulator.Serial) {
                continue
            }
            if (-not $ExpectedAvdName -or $emulator.AvdName -eq $ExpectedAvdName) {
                return $emulator
            }
        }
    } while ((Get-Date) -lt $deadline)

    throw "Timed out waiting for emulator '$ExpectedAvdName' to appear in adb."
}

function Wait-ForBootCompleted {
    param(
        [string]$AdbPath,
        [string]$Serial,
        [int]$TimeoutSeconds = 180
    )

    & $AdbPath -s $Serial wait-for-device | Out-Null

    $deadline = (Get-Date).AddSeconds($TimeoutSeconds)
    do {
        Start-Sleep -Seconds 5
        $bootRaw = & $AdbPath -s $Serial shell getprop sys.boot_completed 2>$null
        if ($null -eq $bootRaw) {
            continue
        }

        $boot = ($bootRaw | Out-String).Trim()
        if ($boot -eq "1") {
            return
        }
    } while ((Get-Date) -lt $deadline)

    throw "Timed out waiting for emulator '$Serial' to finish booting."
}

function Invoke-RepoCommand {
    param(
        [string]$FilePath,
        [string[]]$Arguments
    )

    Push-Location $repoRoot
    try {
        & $FilePath @Arguments
        if ($LASTEXITCODE -ne 0) {
            throw "Command failed with exit code ${LASTEXITCODE}: $FilePath $($Arguments -join ' ')"
        }
    } finally {
        Pop-Location
    }
}

function Print-DryRunPlan {
    param([string]$SelectedAvdName)

    Write-Output "Select emulator target: $SelectedAvdName"
    Write-Output "Start emulator if needed: emulator -avd $SelectedAvdName"
    if (-not $SkipTests) {
        Write-Output ".\gradlew.bat $GradleTaskForTests"
    }
    if (-not $SkipBuild) {
        Write-Output ".\gradlew.bat $GradleTaskForBuild"
    }
    Write-Output "adb -s <serial> install -r $ApkPath"
    Write-Output "adb -s <serial> shell am start -W -n $PackageName/.MainActivity"
}

$sdkDir = Resolve-AndroidSdkDir
$adbPath = Resolve-ToolPath -CommandName "adb" -Candidates @(
    (Join-Path $sdkDir "platform-tools\adb.exe")
)
$emulatorPath = Resolve-ToolPath -CommandName "emulator" -Candidates @(
    (Join-Path $sdkDir "emulator\emulator.exe")
)
$gradleWrapper = Join-Path $repoRoot "gradlew.bat"
$resolvedApkPath = Join-Path $repoRoot $ApkPath

$availableAvds = Get-AvailableAvdNames -EmulatorPath $emulatorPath
$selectedAvdName = Resolve-TargetAvdName -RequestedAvdName $AvdName -AvailableAvds $availableAvds

if ($DryRun) {
    Print-DryRunPlan -SelectedAvdName $selectedAvdName
    exit 0
}

Write-Step "Select emulator target"
$runningEmulators = Get-RunningEmulators -AdbPath $adbPath
$targetEmulator = $runningEmulators | Where-Object { $_.AvdName -eq $selectedAvdName } | Select-Object -First 1

if (-not $targetEmulator) {
    Write-Step "Start emulator if needed"
    $existingSerials = @($runningEmulators | ForEach-Object { $_.Serial })
    Start-Process -FilePath $emulatorPath -ArgumentList "-avd", $selectedAvdName, "-no-snapshot-load" | Out-Null
    $targetEmulator = Wait-ForNewEmulator -AdbPath $adbPath -ExpectedAvdName $selectedAvdName -ExistingSerials $existingSerials
} else {
    Write-Step "Reuse running emulator $($targetEmulator.Serial) for $selectedAvdName"
}

Write-Step "Wait for emulator boot completion"
Wait-ForBootCompleted -AdbPath $adbPath -Serial $targetEmulator.Serial

if (-not $SkipTests) {
    Write-Step "Run unit tests"
    Invoke-RepoCommand -FilePath $gradleWrapper -Arguments @($GradleTaskForTests)
}

if (-not $SkipBuild) {
    Write-Step "Build debug APK"
    Invoke-RepoCommand -FilePath $gradleWrapper -Arguments @($GradleTaskForBuild)
}

if (-not (Test-Path $resolvedApkPath)) {
    throw "Expected APK was not found: $resolvedApkPath"
}

Write-Step "Install APK on emulator $($targetEmulator.Serial)"
& $adbPath -s $targetEmulator.Serial install -r $resolvedApkPath
if ($LASTEXITCODE -ne 0) {
    throw "adb install failed with exit code $LASTEXITCODE"
}

Write-Step "Resolve launcher activity"
$resolvedActivityOutput = & $adbPath -s $targetEmulator.Serial shell cmd package resolve-activity --brief $PackageName
if ($LASTEXITCODE -ne 0) {
    throw "Failed to resolve launcher activity for package $PackageName"
}
$resolvedActivity = ($resolvedActivityOutput | Select-Object -Last 1).Trim()
if (-not $resolvedActivity) {
    throw "Launcher activity could not be resolved for package $PackageName"
}

Write-Step "Launch app"
& $adbPath -s $targetEmulator.Serial logcat -c
& $adbPath -s $targetEmulator.Serial shell am start -W -n $resolvedActivity
if ($LASTEXITCODE -ne 0) {
    throw "Failed to launch activity $resolvedActivity"
}

Write-Host ""
Write-Host "Android debug pipeline completed."
Write-Host "Emulator: $selectedAvdName ($($targetEmulator.Serial))"
Write-Host "APK: $resolvedApkPath"
Write-Host "Activity: $resolvedActivity"
