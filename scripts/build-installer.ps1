# NSIS installer: tray + CLI + ffmpeg + runtime DLLs. Output: target\release\bundle\nsis\
$ErrorActionPreference = "Stop"
$root = Resolve-Path "$PSScriptRoot\.."
. "$PSScriptRoot\env.ps1"
Push-Location "$root\apps\tray\src-tauri"
try { cargo tauri build | Out-Host; if ($LASTEXITCODE) { throw "tauri build failed" } } finally { Pop-Location }
Get-ChildItem "$root\target\release\bundle\nsis" -Filter *.exe | Select-Object FullName, Length
