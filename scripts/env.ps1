# Build environment for this machine: Rust, gcc, clang and Vulkan from MSYS2 UCRT64 (docs/DECISIONS.md, D-001).
# Usage: . .\scripts\env.ps1
$ucrt = "C:\msys64\ucrt64"
if (-not (Test-Path "$ucrt\bin\cargo.exe")) { throw "MSYS2 UCRT64 Rust not found in $ucrt" }
$env:PATH = "$ucrt\bin;$env:PATH"
$env:LIBCLANG_PATH = "$ucrt\bin"
$env:VULKAN_SDK = $ucrt
$env:CMAKE_GENERATOR = "Ninja"
