$ErrorActionPreference = 'Stop'

$toolsDir = "$(Split-Path -Parent $MyInvocation.MyCommand.Definition)"

$packageArgs = @{
  packageName    = $env:ChocolateyPackageName
  unzipLocation  = $toolsDir
  url64bit       = 'https://github.com/qytech/adbx-rs/releases/download/v0.0.4/adbx-v0.0.4-x86_64-pc-windows-msvc.zip'
  checksum64     = '48bcc45fe230d2b833a233dd20e67dc426161a547774e6fbe690a2fec52f0e92'
  checksumType64 = 'sha256'
}

Install-ChocolateyZipPackage @packageArgs
