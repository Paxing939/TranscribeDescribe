# Downloads the prebuilt sherpa-onnx shared libs (MSVC import libs + DLLs) into third_party/sherpa-onnx-lib (D-006).
# GNU ld links the MSVC import libs directly; the C API is plain C, so the MSVC-built DLLs work with MinGW exe.
param([string]$Version = "1.13.8")
$ErrorActionPreference = "Stop"
$root = Resolve-Path "$PSScriptRoot\.."
$dest = "$root\third_party\sherpa-onnx-lib"
if (Test-Path "$dest\sherpa-onnx-c-api.dll") { Write-Host "sherpa-onnx libs already in $dest"; return }
$name = "sherpa-onnx-v$Version-win-x64-shared-MT-Release-lib"
$tmp = Join-Path ([IO.Path]::GetTempPath()) "callscribe-sherpa-$([guid]::NewGuid().ToString('N'))"
New-Item -ItemType Directory -Force $tmp | Out-Null
try {
  $arc = "$tmp\$name.tar.bz2"
  Invoke-WebRequest "https://github.com/k2-fsa/sherpa-onnx/releases/download/v$Version/$name.tar.bz2" -OutFile $arc
  tar -xjf $arc -C $tmp
  if ($LASTEXITCODE) { throw "tar failed" }
  New-Item -ItemType Directory -Force $dest | Out-Null
  Copy-Item "$tmp\$name\lib\*" $dest -Force
} finally { Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue }
Write-Host "sherpa-onnx $Version libs: $dest"
