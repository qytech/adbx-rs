$ErrorActionPreference = 'Stop'

$toolsDir = "$(Split-Path -Parent $MyInvocation.MyCommand.Definition)"

$packageArgs = @{
  packageName    = $env:ChocolateyPackageName
  unzipLocation  = $toolsDir
  url64bit       = 'https://github.com/Jax-Lifu/adbx-rs/releases/download/v0.0.1/adbx-v0.0.1-x86_64-pc-windows-msvc.zip'
  checksum64     = 'a080ce8defed0e6cd7b4ff580bf0e5fdd049de72827a4ce7cdd3fd55df5fa666'
  checksumType64 = 'sha256'
}

Install-ChocolateyZipPackage @packageArgs
