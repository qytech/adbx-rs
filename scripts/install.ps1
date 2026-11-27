$ErrorActionPreference = "Stop"

$Repo = if ($env:ADBX_REPO) { $env:ADBX_REPO } else { "qytech/adbx-rs" }
$Version = if ($env:ADBX_VERSION) { $env:ADBX_VERSION } else { "latest" }
$InstallDir = if ($env:ADBX_INSTALL_DIR) { $env:ADBX_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA "adbx\bin" }

if ($Version -eq "latest") {
    $resp = Invoke-WebRequest -UseBasicParsing -Uri "https://github.com/$Repo/releases/latest"
    $finalUrl = $resp.BaseResponse.ResponseUri.AbsoluteUri
    if (-not $finalUrl) { $finalUrl = $resp.BaseResponse.RequestMessage.RequestUri.AbsoluteUri }
    if ($finalUrl -match '/releases/tag/([^/?#]+)') {
        $Version = $Matches[1]
    } else {
        $release = Invoke-RestMethod -Uri "https://api.github.com/repos/$Repo/releases/latest"
        $Version = $release.tag_name
    }
}

$Archive = "adbx-$Version-x86_64-pc-windows-msvc.zip"
$BaseUrl = "https://github.com/$Repo/releases/download/$Version"

$TmpDir = Join-Path ([System.IO.Path]::GetTempPath()) ("adbx-install-" + [System.Guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Path $TmpDir | Out-Null

try {
    $ZipPath = Join-Path $TmpDir $Archive
    $SumsPath = Join-Path $TmpDir "SHA256SUMS.txt"

    Write-Host "Downloading $Archive from $Repo ($Version)..."
    Invoke-WebRequest -Uri "$BaseUrl/$Archive" -OutFile $ZipPath
    Invoke-WebRequest -Uri "$BaseUrl/SHA256SUMS.txt" -OutFile $SumsPath

    $line = Get-Content $SumsPath | Where-Object { $_ -match [regex]::Escape($Archive) } | Select-Object -First 1
    if (-not $line) {
        throw "Checksum entry for $Archive not found in SHA256SUMS.txt"
    }
    $expected = ($line -split '\s+')[0].ToLowerInvariant()
    $actual = (Get-FileHash -Path $ZipPath -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($expected -ne $actual) {
        throw "SHA256 mismatch! expected=$expected actual=$actual"
    }

    Expand-Archive -Path $ZipPath -DestinationPath $TmpDir -Force
    New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
    Copy-Item -Path (Join-Path $TmpDir "adbx.exe") -Destination (Join-Path $InstallDir "adbx.exe") -Force

    $userPath = [Environment]::GetEnvironmentVariable("Path", "User")
    if (-not $userPath) { $userPath = "" }
    if (($userPath -split ';') -notcontains $InstallDir) {
        $newPath = if ($userPath.Trim()) { "$userPath;$InstallDir" } else { $InstallDir }
        [Environment]::SetEnvironmentVariable("Path", $newPath, "User")
        Write-Host "Added $InstallDir to User PATH (restart terminal to take effect)."
    }

    Write-Host "Installed adbx.exe to $(Join-Path $InstallDir 'adbx.exe')"
    if (-not (Get-Command adb -ErrorAction SilentlyContinue)) {
        Write-Host "Note: 'adb' was not found in PATH. Run 'adbx setup-adb' to auto-download official Android Platform-Tools."
    }
    Write-Host 'Enable PowerShell completion in $PROFILE:'
    Write-Host '  $env:COMPLETE = "powershell"; adbx | Out-String | Invoke-Expression; Remove-Item Env:\COMPLETE'
} finally {
    Remove-Item -Path $TmpDir -Recurse -Force -ErrorAction SilentlyContinue
}
