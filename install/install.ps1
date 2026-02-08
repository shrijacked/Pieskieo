# Pieskieo installer (Windows PowerShell)
# Downloads prebuilt release zip, installs into %ProgramData%\Pieskieo\bin (or $env:PIESKIEO_PREFIX)
# Usage: iwr https://raw.githubusercontent.com/DarsheeeGamer/Pieskieo/main/install/install.ps1 -UseBasicParsing | iex

param(
    [string]$Version,
    [string]$Prefix
)

function Get-LatestTag {
    try {
        $r = Invoke-WebRequest -UseBasicParsing -Uri "https://github.com/DarsheeeGamer/Pieskieo/releases/latest" -MaximumRedirection 0 -ErrorAction Stop
    } catch {
        $r = $_.Exception.Response
    }
    $loc = $r.Headers["Location"]
    if ($loc -match "/tag/(v[^/]+)") { return $Matches[1] }
    return "v1.0.0"
}

$tag = if ($Version) { $Version } elseif ($env:PIESKIEO_VERSION) { $env:PIESKIEO_VERSION } else { Get-LatestTag }
$plat = "windows-x86_64"
$url = "https://github.com/DarsheeeGamer/Pieskieo/releases/download/$tag/pieskieo-$plat-$tag.zip"

$dstPrefix = if ($Prefix) { $Prefix } elseif ($env:PIESKIEO_PREFIX) { $env:PIESKIEO_PREFIX } elseif ($env:ProgramData) { Join-Path $env:ProgramData "Pieskieo" } else { Join-Path $HOME ".local" }
$bindst = Join-Path $dstPrefix "bin"
New-Item -ItemType Directory -Force -Path $bindst | Out-Null

$tmp = New-Item -ItemType Directory -Path ([System.IO.Path]::GetTempPath()) -Name ("pieskieo-" + [System.Guid]::NewGuid().ToString())
$zip = Join-Path $tmp "pieskieo.zip"

Write-Host "Downloading $url"
Invoke-WebRequest -UseBasicParsing -Uri $url -OutFile $zip
Expand-Archive -Path $zip -DestinationPath $tmp -Force

Get-ChildItem $tmp -Filter "pieskieo*.exe" | ForEach-Object {
  Copy-Item $_.FullName $bindst -Force
}
Write-Host "Installed to $bindst"
Write-Host "Ensure $bindst is on PATH."
