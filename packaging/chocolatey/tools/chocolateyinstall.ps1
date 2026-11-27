$ErrorActionPreference = 'Stop'

$toolsDir = "$(Split-Path -Parent $MyInvocation.MyCommand.Definition)"

$packageArgs = @{
  packageName    = $env:ChocolateyPackageName
  unzipLocation  = $toolsDir
  url64bit       = 'https://github.com/Jax-Lifu/adbx-rs/releases/download/v0.0.2/adbx-v0.0.2-x86_64-pc-windows-msvc.zip'
  checksum64     = '25b6b914d402f82f9a46511b358b2d68136a5289374d0b9342a55b0ab50d0faf'
  checksumType64 = 'sha256'
}

Install-ChocolateyZipPackage @packageArgs
