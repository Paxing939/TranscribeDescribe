; Removes what the app wrote itself (autostart, PATH) before uninstalling (§11, п. 37).
!macro NSIS_HOOK_PREUNINSTALL
  ExecWait '"$INSTDIR\callscribe.exe" cleanup'
!macroend
