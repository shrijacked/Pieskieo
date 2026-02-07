$ErrorActionPreference = "Stop"

# Auto-fetch prebuilt Pieskieo zip from GitHub releases.
# Usage (PowerShell):
#   iwr https://raw.githubusercontent.com/DarsheeeGamer/Pieskieo/main/install/get-pieskieo.ps1 -UseBasicParsing | iex
# Optional env:
#   PIESKIEO_VERSION   tag to install (default v0.1.2)
#   PIESKIEO_PREFIX    install prefix (default: %ProgramData%\Pieskieo or $HOME\.local)

function Choose-Prefix {
  if ($env:PIESKIEO_PREFIX) { return $env:PIESKIEO_PREFIX }
  if ($env:ProgramData) { return (Join-Path $env:ProgramData "Pieskieo") }
  return (Join-Path $HOME ".local")
}

function Main {
  $version = $env:PIESKIEO_VERSION
  if (-not $version) { $version = "v0.1.2" }
  $platform = "windows-x86_64"
  $url = "https://github.com/DarsheeeGamer/Pieskieo/releases/download/$version/pieskieo-$platform-$version.zip"
  Write-Host "Downloading $url"

  $tmp = New-Item -ItemType Directory -Path ([System.IO.Path]::GetTempPath()) -Name ("pieskieo-" + [System.Guid]::NewGuid().ToString()) -Force
  $zip = Join-Path $tmp "pieskieo.zip"
  Invoke-WebRequest -UseBasicParsing -Uri $url -OutFile $zip

  $prefix = Choose-Prefix
  $binDst = Join-Path $prefix "bin"
  New-Item -ItemType Directory -Force -Path $binDst | Out-Null

  Expand-Archive -Path $zip -DestinationPath $tmp -Force
  Get-ChildItem -Path $tmp -Filter "pieskieo*.exe" | ForEach-Object {
    Copy-Item $_.FullName $binDst -Force
  }
  Write-Host "Installed binaries to $binDst"
  Write-Host "Ensure $binDst is on your PATH."
}

Main
