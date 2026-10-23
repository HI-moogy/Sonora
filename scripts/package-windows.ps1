# Builds Sonora for Windows as one portable file: dist\Sonora.exe.
#
# The C runtime is linked in, so it runs without the Visual C++
# redistributable, and the open-source licenses are built in (About and bug
# reports > Open-source licenses). The name stays the same between versions,
# so replacing the file updates Sonora and keeps Start with Windows working.
# Needs Rust (MSVC toolchain) and Python 3 (for the licenses).
$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent $PSScriptRoot
Set-Location $root
$targetDir = Join-Path $root 'target\package-windows'
New-Item -ItemType Directory -Force $targetDir, (Join-Path $root 'dist') | Out-Null

$notices = Join-Path $targetDir 'THIRD-PARTY-NOTICES.txt'
python scripts/third_party_notices.py --target x86_64-pc-windows-msvc | Out-File -Encoding utf8 $notices
if ($LASTEXITCODE -ne 0) { throw 'third-party notices failed' }

$env:SONORA_NOTICES = $notices
$env:RUSTFLAGS = '-C target-feature=+crt-static'
cargo build --release --locked -p app --target-dir $targetDir
if ($LASTEXITCODE -ne 0) { throw 'cargo build failed' }

$exe = Join-Path $root 'dist\Sonora.exe'
Copy-Item -Force (Join-Path $targetDir 'release\sonora.exe') $exe
$exe
