$ErrorActionPreference = 'Stop'

$toolsDir = "$(Split-Path -Parent $MyInvocation.MyCommand.Definition)"

$packageArgs = @{
  packageName    = $env:ChocolateyPackageName
  unzipLocation  = $toolsDir
  url64bit       = 'https://github.com/qytech/adbx-rs/releases/download/v0.0.3/adbx-v0.0.3-x86_64-pc-windows-msvc.zip'
  checksum64     = 'd6626904a4378aafcbe80c0066399c22dc28d6e36a8b65b3bec361ee97adeab0'
  checksumType64 = 'sha256'
}

Install-ChocolateyZipPackage @packageArgs
