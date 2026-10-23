# Builds Sonora for Windows and packs it as dist\sonora-<version>-windows-x64.zip.
#
# The C runtime is linked in, so the exe runs without the Visual C++
# redistributable. Needs Rust (MSVC toolchain) and Python 3 (for the
# third-party notices).
$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent $PSScriptRoot
Set-Location $root
$version = (Select-String -Path 'crates\app\Cargo.toml' -Pattern '^version = "(.*)"').Matches[0].Groups[1].Value
$targetDir = Join-Path $root 'target\package-windows'

$env:RUSTFLAGS = '-C target-feature=+crt-static'
cargo build --release --locked -p app --target-dir $targetDir
if ($LASTEXITCODE -ne 0) { throw 'cargo build failed' }

$name = "sonora-$version-windows-x64"
$stage = Join-Path ([System.IO.Path]::GetTempPath()) $name
if (Test-Path $stage) { Remove-Item -Recurse -Force $stage }
New-Item -ItemType Directory -Force $stage | Out-Null
New-Item -ItemType Directory -Force (Join-Path $root 'dist') | Out-Null

Copy-Item (Join-Path $targetDir 'release\sonora.exe') $stage
Copy-Item README.md, CHANGELOG.md, LICENSE $stage
python scripts/third_party_notices.py --target x86_64-pc-windows-msvc | Out-File -Encoding utf8 (Join-Path $stage 'THIRD-PARTY-NOTICES.txt')
if ($LASTEXITCODE -ne 0) { throw 'third-party notices failed' }

$zip = Join-Path $root "dist\$name.zip"
if (Test-Path $zip) { Remove-Item -Force $zip }
Compress-Archive -Path (Join-Path $stage '*') -DestinationPath $zip
Remove-Item -Recurse -Force $stage
$zip
