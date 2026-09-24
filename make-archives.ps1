# ---------------------------------------------------------------------------
# Build the source distribution archives (Windows zip / Linux tar.gz).
#
# Called by package.bat steps 2 and 3:
#   powershell -File make-archives.ps1 -Only windows
#   powershell -File make-archives.ps1 -Only linux
#
# Why a separate file: those two steps used to be two ~2000-char -EncodedCommand
# Base64 blobs inside package.bat -- impossible to review or edit, and on failure
# all you got was a bare "XML parse error". The logic lives here now, in plain
# text, and can be run on its own.
#
# Usage:
#   pwsh -File make-archives.ps1                 # both archives
#   pwsh -File make-archives.ps1 -Only windows   # Windows source zip only
#   pwsh -File make-archives.ps1 -Only linux     # Linux source tarball only
#
# ENCODING: this file is deliberately pure ASCII with no BOM.
# package.bat invokes it with Windows PowerShell 5.1, which reads a BOM-less file
# as ANSI -- any non-ASCII byte here would corrupt the script and produce
# "Unexpected token" parse errors. Keep it ASCII-only so it survives being
# edited by tools that strip the BOM. (The dev-side wrapper in .dev\ is a
# separate file and may contain any characters, since it runs under pwsh 7.)
# ---------------------------------------------------------------------------
param(
  [ValidateSet('all', 'windows', 'linux')]
  [string]$Only = 'all'
)
$ErrorActionPreference = 'Stop'

# Project root: normally the directory holding this script; the .dev\ wrapper
# points here explicitly via DSH_ARCHIVES_PROJ when run from the workspace.
if ($env:DSH_ARCHIVES_PROJ) {
  Set-Location $env:DSH_ARCHIVES_PROJ
} else {
  Set-Location $PSScriptRoot
}

# Single source of truth for the version: Cargo.toml
$m = [regex]::Match((Get-Content 'Cargo.toml' -Raw), '(?m)^\s*version\s*=\s*"([^"]+)"')
if (-not $m.Success) { throw 'Cannot parse version from Cargo.toml' }
$ver = $m.Groups[1].Value
New-Item -ItemType Directory -Force -Path 'Output' | Out-Null

# Shared content: sources plus everything needed to build (no target/, no Output/).
# The .bat helpers are discovered rather than named, so a renamed or added script
# cannot silently drop out of the archives (one of them has a non-ASCII name,
# which would also drag this file back into an encoding dependency).
$bat = @(Get-ChildItem -File -Filter '*.bat' | Select-Object -ExpandProperty Name)
$common = @('Cargo.toml', 'Cargo.lock', 'build.rs', 'icon.ico', 'icon.svg',
            'README.md', 'installer.iss', 'install.sh', 'build-linux.sh')

if ($Only -in @('all', 'windows')) {
  Write-Output '[2/3] Windows source archive ...'
  $zip = "Output\DSH_Launch_Console-$ver-windows-src.zip"
  Remove-Item $zip -Force -ErrorAction SilentlyContinue
  $winItems = @('src') + $common + $bat + @('make-archives.ps1', 'docs')
  Compress-Archive -Path $winItems -DestinationPath $zip -Force
  Write-Output ("      done: " + $zip)
}

if ($Only -in @('all', 'linux')) {
  Write-Output '[3/3] Linux source archive ...'
  $stageRoot = Join-Path $env:TEMP 'dsh-tar'
  $stage = Join-Path $stageRoot "DSH_Launch_Console-$ver"
  Remove-Item $stageRoot -Recurse -Force -ErrorAction SilentlyContinue
  New-Item -ItemType Directory -Force -Path (Join-Path $stage 'src') | Out-Null
  Copy-Item $common $stage -Force
  Copy-Item 'src\*' (Join-Path $stage 'src') -Force
  # README screenshots: without them the README in the tarball has broken images
  Copy-Item 'docs' $stage -Recurse -Force

  # Note on hashes: the archives are NOT byte-reproducible -- Rust builds are not,
  # NTFS returns directory entries in varying order, and gzip stores a timestamp.
  # Chasing that is not worth it. Instead every run writes Output\SHA256SUMS.txt
  # (see the end of this script), so the published checksums are generated from
  # the exact files being shipped instead of being copied into docs by hand.
  $tar = "Output\DSH_Launch_Console-$ver-linux.tar.gz"
  Remove-Item $tar -Force -ErrorAction SilentlyContinue
  & tar -czf $tar -C $stageRoot "DSH_Launch_Console-$ver"
  if ($LASTEXITCODE -ne 0) { throw "tar failed with exit code $LASTEXITCODE" }
  Remove-Item $stageRoot -Recurse -Force -ErrorAction SilentlyContinue

  # Self-check: archive lists, and every entry sits under the versioned top dir
  # (install.sh keeps its LF endings; tar does not rewrite them).
  $listing = & tar -tzf $tar
  if ($LASTEXITCODE -ne 0) { throw 'tar -tzf failed (archive may be corrupt)' }
  $bad = $listing | Where-Object { $_ -notmatch "^DSH_Launch_Console-$ver/" }
  if ($bad) { throw ('unexpected top-level entries: ' + ($bad -join ', ')) }
  Write-Output ("      done: " + $tar + " (" + @($listing).Count + " entries)")
}

# Checksums for whatever is now in Output for this version. Generated, never
# hand-copied: the files are not byte-reproducible (see the note above), so a
# checksum pasted into the release notes goes stale the moment you repackage.
#
# SHA-256 is computed with .NET, not Get-FileHash. package.bat calls this through
# Windows PowerShell 5.1, and when PSModulePath carries PowerShell 7 module
# directories (the pwsh/MSIX install prepends them) 5.1 cannot load
# Microsoft.PowerShell.Utility at all -- a bare *or* module-qualified Get-FileHash
# both fail with "not recognized". The .NET API has no such dependency, so it
# works no matter how PSModulePath is mangled.
function Get-Sha256([string]$Path) {
  $sha = [System.Security.Cryptography.SHA256]::Create()
  try {
    $fs = [System.IO.File]::OpenRead($Path)
    try {
      ($sha.ComputeHash($fs) | ForEach-Object { $_.ToString('X2') }) -join ''
    } finally { $fs.Dispose() }
  } finally { $sha.Dispose() }
}

$sums = "Output\SHA256SUMS.txt"
$lines = Get-ChildItem 'Output' -File |
  Where-Object { $_.Name -like "*$ver*" } |
  Sort-Object Name |
  ForEach-Object { (Get-Sha256 $_.FullName) + '  ' + $_.Name }
Set-Content -Path $sums -Value $lines -Encoding ASCII
Write-Output ''
Write-Output "=== $sums ==="
$lines | ForEach-Object { Write-Output ("  " + $_) }
Write-Output 'ARCHIVES-DONE'
