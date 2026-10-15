# Runs before `cargo tauri build` (beforeBuildCommand): frontend, CLI, sidecars, runtime DLLs (§11 п. 31, D-001).
$ErrorActionPreference = "Stop"
$root = Resolve-Path "$PSScriptRoot\.."
. "$PSScriptRoot\env.ps1"

Push-Location "$root\apps\tray\ui"
try {
  if (-not (Test-Path node_modules)) { npm install --no-fund --no-audit | Out-Host }
  npm run build | Out-Host
} finally { Pop-Location }

# sherpa-onnx prebuilt libs (D-006): needed to link the CLI with the `sherpa` feature.
& "$PSScriptRoot\fetch-sherpa.ps1"

Push-Location $root
try { cargo build --release -p callscribe-cli | Out-Host; if ($LASTEXITCODE) { throw "cli build failed" } } finally { Pop-Location }

$triple = (rustc -vV | Select-String "^host:").ToString().Split(" ")[1]
$bin = "$root\apps\tray\src-tauri\binaries"
New-Item -ItemType Directory -Force $bin | Out-Null
Copy-Item "$root\target\release\callscribe.exe" "$bin\callscribe-$triple.exe" -Force
foreach ($t in "ffmpeg", "ffprobe") {
  $src = (Get-Command "$t.exe" -ErrorAction Stop).Source
  Copy-Item $src "$bin\$t-$triple.exe" -Force
}

# MinGW runtime DLLs needed by callscribe.exe / callscribe-tray.exe.
$rt = "$root\apps\tray\src-tauri\runtime"
New-Item -ItemType Directory -Force $rt | Out-Null
foreach ($d in "libstdc++-6.dll", "libgcc_s_seh-1.dll", "libwinpthread-1.dll") {
  $p = "C:\msys64\ucrt64\bin\$d"
  if (Test-Path $p) { Copy-Item $p $rt -Force }
}
# sherpa-onnx DLLs (GigaAM, Parakeet, diarization; D-006).
foreach ($d in "sherpa-onnx-c-api.dll", "onnxruntime.dll", "onnxruntime_providers_shared.dll") {
  Copy-Item "$root\third_party\sherpa-onnx-lib\$d" $rt -Force
}
Write-Host "prebuild done ($triple)"
