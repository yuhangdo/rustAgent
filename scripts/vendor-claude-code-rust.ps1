param(
    [string]$Source = ".tmp\claude-code-rust",
    [string]$Destination = "native\claude-code-rust"
)

$ErrorActionPreference = "Stop"

if (!(Test-Path $Source)) {
    throw "Source repo not found: $Source"
}

New-Item -ItemType Directory -Force (Split-Path $Destination -Parent) | Out-Null

robocopy $Source $Destination /E /XD ".git" "target" "claude-code-main (2)" "claude-code-rev-main" /XF ".env" | Out-Null

if ($LASTEXITCODE -ge 8) {
    throw "robocopy failed with exit code $LASTEXITCODE"
}

@(
    Join-Path $Destination "claude-code-main (2)"
    Join-Path $Destination "claude-code-rev-main"
) | ForEach-Object {
    if (Test-Path $_) {
        Remove-Item -LiteralPath $_ -Recurse -Force
    }
}

Write-Output "Vendored claude-code-rust Rust implementation into $Destination"
