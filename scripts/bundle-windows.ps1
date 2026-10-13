# Build a Windows release archive containing the executable.
$ErrorActionPreference = "Stop"
Set-Location (Join-Path $PSScriptRoot "..")
cargo build --locked --release
if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }
$architecture = if ($env:PROCESSOR_ARCHITECTURE -eq "ARM64") { "aarch64" } else { "x86_64" }
$name = "nightshift-windows-$architecture"
$stage = "target/release/$name"
if (Test-Path $stage) { Remove-Item -Recurse -Force $stage }
New-Item -ItemType Directory -Path $stage | Out-Null
Copy-Item target/release/nightshift.exe "$stage/nightshift.exe"
$archive = "target/release/$name.zip"
if (Test-Path $archive) { Remove-Item -Force $archive }
Compress-Archive -Path $stage -DestinationPath $archive
Write-Output $archive
