param(
    [string]$Target = "arm64-v8a",
    [switch]$Debug
)

$ErrorActionPreference = "Stop"

$repoRoot = Split-Path -Parent $PSScriptRoot
$crateDir = Join-Path $repoRoot "native\claude-code-rust"
$jniRoot = Join-Path $repoRoot "app\src\main\jniLibs"

function Resolve-ExecutablePath {
    param(
        [string]$CommandName,
        [string[]]$CandidatePaths = @()
    )

    $resolved = Get-Command $CommandName -ErrorAction SilentlyContinue
    if ($resolved) {
        return $resolved.Source
    }

    foreach ($candidate in $CandidatePaths) {
        if ($candidate -and (Test-Path $candidate)) {
            return (Resolve-Path $candidate).Path
        }
    }

    return $null
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

function Ensure-RustTargetInstalled {
    param(
        [string]$RustupPath,
        [string]$CargoTarget
    )

    if (-not $RustupPath) {
        return
    }

    $installedTargets = & $RustupPath target list --installed
    if ($LASTEXITCODE -ne 0) {
        throw "rustup target list failed with exit code $LASTEXITCODE."
    }

    if ($installedTargets -contains $CargoTarget) {
        return
    }

    Write-Host "Rust target $CargoTarget is not installed. Installing with rustup..."
    & $RustupPath target add $CargoTarget
    if ($LASTEXITCODE -ne 0) {
        throw "rustup target add $CargoTarget failed with exit code $LASTEXITCODE."
    }
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

function Resolve-AndroidNdkDir {
    param([string]$SdkDir)

    $directCandidates = @(
        $env:ANDROID_NDK_HOME,
        $env:ANDROID_NDK_ROOT
    ) | Where-Object { $_ }

    foreach ($candidate in $directCandidates) {
        if (Test-Path $candidate) {
            return (Resolve-Path $candidate).Path
        }
    }

    $ndkRoot = Join-Path $SdkDir "ndk"
    if (-not (Test-Path $ndkRoot)) {
        throw "Android NDK was not found under $SdkDir. Install an NDK or set ANDROID_NDK_HOME."
    }

    $latestNdk = Get-ChildItem $ndkRoot -Directory |
        Sort-Object Name -Descending |
        Select-Object -First 1

    if (-not $latestNdk) {
        throw "Android NDK was not found under $ndkRoot."
    }

    return $latestNdk.FullName
}

function Resolve-LlvmMingwBin {
    $resolved = Get-Command "x86_64-w64-mingw32-clang" -ErrorAction SilentlyContinue
    if ($resolved) {
        return Split-Path -Parent $resolved.Source
    }

    $candidateRoots = @()
    if ($env:LLVM_MINGW_HOME) {
        $candidateRoots += $env:LLVM_MINGW_HOME
    }

    $repoToolchainsRoot = Join-Path $repoRoot ".tmp\toolchains"
    if (Test-Path $repoToolchainsRoot) {
        $candidateRoots += (
            Get-ChildItem $repoToolchainsRoot -Directory -Filter "llvm-mingw-*" |
                Sort-Object Name -Descending |
                ForEach-Object { $_.FullName }
        )
    }

    foreach ($root in $candidateRoots) {
        $binDir = Join-Path $root "bin"
        if (Test-Path (Join-Path $binDir "x86_64-w64-mingw32-clang.exe")) {
            return $binDir
        }
    }

    return $null
}

$targetConfig = switch ($Target) {
    "arm64-v8a" {
        @{
            Abi = "arm64-v8a"
            CargoTarget = "aarch64-linux-android"
            ClangExecutable = "aarch64-linux-android26-clang.cmd"
            TargetEnvPrefix = "AARCH64_LINUX_ANDROID"
            CcEnvName = "CC_aarch64_linux_android"
            ArEnvName = "AR_aarch64_linux_android"
        }
    }
    "x86_64" {
        @{
            Abi = "x86_64"
            CargoTarget = "x86_64-linux-android"
            ClangExecutable = "x86_64-linux-android26-clang.cmd"
            TargetEnvPrefix = "X86_64_LINUX_ANDROID"
            CcEnvName = "CC_x86_64_linux_android"
            ArEnvName = "AR_x86_64_linux_android"
        }
    }
    default {
        throw "Unsupported Android ABI target: $Target"
    }
}

$cargoPath = Resolve-ExecutablePath "cargo" @(
    (Join-Path $env:USERPROFILE ".cargo\bin\cargo.exe")
)
if (-not $cargoPath) {
    throw "cargo was not found. Install Rust or expose %USERPROFILE%\\.cargo\\bin to PATH."
}
$rustupPath = Resolve-ExecutablePath "rustup" @(
    (Join-Path $env:USERPROFILE ".cargo\bin\rustup.exe")
)
Ensure-RustTargetInstalled -RustupPath $rustupPath -CargoTarget $targetConfig.CargoTarget

$sdkDir = Resolve-AndroidSdkDir
$ndkDir = Resolve-AndroidNdkDir -SdkDir $sdkDir
$ndkBinDir = Join-Path $ndkDir "toolchains\llvm\prebuilt\windows-x86_64\bin"
if (-not (Test-Path $ndkBinDir)) {
    throw "Android NDK LLVM toolchain bin directory was not found: $ndkBinDir"
}

$llvmMingwBinDir = Resolve-LlvmMingwBin
if (-not $llvmMingwBinDir) {
    throw "x86_64-w64-mingw32-clang was not found. Install LLVM MinGW, set LLVM_MINGW_HOME, or place it under $repoRoot\\.tmp\\toolchains."
}

$hostLinker = Join-Path $llvmMingwBinDir "x86_64-w64-mingw32-clang.exe"
$targetLinker = Join-Path $ndkBinDir $targetConfig.ClangExecutable
$targetAr = Join-Path $ndkBinDir "llvm-ar.exe"
if (-not (Test-Path $targetLinker)) {
    throw "Android target linker was not found: $targetLinker"
}
if (-not (Test-Path $targetAr)) {
    throw "Android target archiver was not found: $targetAr"
}

$cargoBinDir = Split-Path -Parent $cargoPath
$env:PATH = "$llvmMingwBinDir;$cargoBinDir;$ndkBinDir;$env:PATH"
$env:CARGO_TARGET_X86_64_PC_WINDOWS_GNULLVM_LINKER = $hostLinker
Set-Item -Path "Env:$($targetConfig.CcEnvName)" -Value $targetLinker
Set-Item -Path "Env:$($targetConfig.ArEnvName)" -Value $targetAr
Set-Item -Path "Env:CARGO_TARGET_$($targetConfig.TargetEnvPrefix)_LINKER" -Value $targetLinker

$profile = if ($Debug) { "debug" } else { "release" }
$outDir = Join-Path $jniRoot $targetConfig.Abi
New-Item -ItemType Directory -Force -Path $outDir | Out-Null

Push-Location $crateDir
try {
    $cargoArgs = @(
        "rustc",
        "--lib",
        "--target", $targetConfig.CargoTarget,
        "--no-default-features",
        "--features", "mobile-bridge",
        "--",
        "--crate-type", "cdylib"
    )

    if (-not $Debug) {
        $cargoArgs += "--release"
    }

    & $cargoPath @cargoArgs
    if ($LASTEXITCODE -ne 0) {
        throw "cargo build failed with exit code $LASTEXITCODE."
    }

    $builtLibPath = Join-Path $crateDir "target\$($targetConfig.CargoTarget)\$profile\libclaude_code_rs.so"
    if (-not (Test-Path $builtLibPath)) {
        throw "Expected native library was not produced: $builtLibPath"
    }

    $packagedLibPath = Join-Path $outDir "libclaude_code_rs.so"
    Copy-Item $builtLibPath $packagedLibPath -Force

    Write-Host "Embedded Rust agent built for $Target and copied to $packagedLibPath."
}
finally {
    Pop-Location
}
