# Build the Windows .exe on any Windows machine with the stable Rust toolchain.
#   1) install Rust:  winget install Rustlang.rustup  (or https://rustup.rs)
#   2) in PowerShell:  pwsh build-windows.ps1
$ErrorActionPreference = "Stop"

# Make sure the MSVC target is present (needed for egui/eframe on Windows)
rustup target add x86_64-pc-windows-msvc 2>$null

Write-Host "== engine tests ==" -ForegroundColor Cyan
cargo test --lib --release
if ($LASTEXITCODE -ne 0) { throw "engine tests failed" }

Write-Host "== release build ==" -ForegroundColor Cyan
cargo build --release --target x86_64-pc-windows-msvc
if ($LASTEXITCODE -ne 0) { throw "build failed" }

$exe = "target\x86_64-pc-windows-msvc\release\xray-viewer.exe"
Copy-Item $exe ".\xray-viewer.exe" -Force
Get-Item ".\xray-viewer.exe" | Format-List Name, Length, LastWriteTime
Write-Host "== done: .\xray-viewer.exe ==" -ForegroundColor Green
# Optional: smoke test the exe on this Windows machine
& ".\xray-viewer.exe"
