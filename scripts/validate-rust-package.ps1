param(
    [string]$PackageDir = "dist\rust-package",
    [string]$CliBinary = "target\release\spotifydl-cli.exe",
    [string]$GuiBinary = "target\release\spotifydl-gui.exe",
    [string]$DownloaderBinary = "",
    [switch]$SkipBuild,
    [switch]$SkipGuiCopy,
    [switch]$JsonStatus
)

$ErrorActionPreference = "Stop"

function Resolve-RepoPath {
    param([string]$RelativePath)

    $root = Split-Path -Parent $PSScriptRoot
    [System.IO.Path]::GetFullPath((Join-Path $root $RelativePath))
}

function Ensure-ParentDirectory {
    param([string]$Path)

    $parent = Split-Path -Parent $Path
    if (-not [string]::IsNullOrWhiteSpace($parent)) {
        New-Item -ItemType Directory -Force -Path $parent | Out-Null
    }
}

function Copy-BinaryIfPresent {
    param(
        [string]$SourcePath,
        [string]$DestinationPath,
        [string]$Label
    )

    if (-not (Test-Path -LiteralPath $SourcePath)) {
        throw "$Label not found: $SourcePath"
    }

    Ensure-ParentDirectory -Path $DestinationPath
    Copy-Item -LiteralPath $SourcePath -Destination $DestinationPath -Force
}

$packageRoot = Resolve-RepoPath $PackageDir
$cliSource = Resolve-RepoPath $CliBinary
$guiSource = Resolve-RepoPath $GuiBinary

if (-not $SkipBuild) {
    Write-Host "Building Rust release binaries..."
    cargo build --release -p spotifydl-cli -p spotifydl-gui
}

New-Item -ItemType Directory -Force -Path $packageRoot | Out-Null

$cliDestination = Join-Path $packageRoot "spotifydl-cli.exe"
Copy-BinaryIfPresent -SourcePath $cliSource -DestinationPath $cliDestination -Label "CLI binary"

if (-not $SkipGuiCopy) {
    $guiDestination = Join-Path $packageRoot "spotifydl-gui.exe"
    Copy-BinaryIfPresent -SourcePath $guiSource -DestinationPath $guiDestination -Label "GUI binary"
}

if ([string]::IsNullOrWhiteSpace($DownloaderBinary)) {
    $candidatePaths = @(
        (Resolve-RepoPath "dist\spotify-dl-gui\spotify-dl.exe"),
        (Resolve-RepoPath "spotify-dl.exe")
    )

    $DownloaderBinary = $candidatePaths | Where-Object { Test-Path -LiteralPath $_ } | Select-Object -First 1
}
else {
    $DownloaderBinary = Resolve-RepoPath $DownloaderBinary
}

if ([string]::IsNullOrWhiteSpace($DownloaderBinary)) {
    throw "Downloader binary was not provided and no default spotify-dl.exe candidate was found."
}

$downloaderDestination = Join-Path $packageRoot "spotify-dl.exe"
Copy-BinaryIfPresent -SourcePath $DownloaderBinary -DestinationPath $downloaderDestination -Label "Downloader binary"

$validationDatabase = Join-Path $packageRoot "validation.sqlite"
if (Test-Path -LiteralPath $validationDatabase) {
    Remove-Item -LiteralPath $validationDatabase -Force
}

$statusArgs = @("status", "--require-ready", "--database", $validationDatabase)
if ($JsonStatus) {
    $statusArgs += "--json"
}

Write-Host "Running packaged CLI readiness check..."
& $cliDestination @statusArgs
if ($LASTEXITCODE -ne 0) {
    throw "Packaged CLI readiness check failed with exit code $LASTEXITCODE."
}

Write-Host ""
Write-Host "Packaged layout validated:"
Write-Host "  Package: $packageRoot"
Write-Host "  CLI: $cliDestination"
if (-not $SkipGuiCopy) {
    Write-Host "  GUI: $(Join-Path $packageRoot 'spotifydl-gui.exe')"
}
Write-Host "  Downloader: $downloaderDestination"
Write-Host "  Validation DB: $validationDatabase"
