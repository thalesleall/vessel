; Hooks for the Windows installer (bundle.windows.nsis.installerHooks in tauri.conf.json).

; vessel-mcp.exe, the bridge for AI assistants, is started and kept running by the assistant
; (Claude Code, Claude Desktop, Cursor...), not by Vessel. Closing Vessel does not free it, and
; the installer could not overwrite the file ("Erro ao abrir o arquivo pra gravação"). Windows
; does let a running program's file be renamed: the old copy is moved aside, the new one is
; written in its place, and a running bridge keeps working until the assistant restarts it.
; Copies moved aside are deleted by the next install or by the uninstaller.
!macro NSIS_HOOK_PREINSTALL
  Delete "$INSTDIR\vessel-mcp-old-*.exe"
  ${If} ${FileExists} "$INSTDIR\vessel-mcp.exe"
    Push $R9
    System::Call "kernel32::GetTickCount() i .R9"
    Rename "$INSTDIR\vessel-mcp.exe" "$INSTDIR\vessel-mcp-old-$R9.exe"
    Pop $R9
  ${EndIf}
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  Delete /REBOOTOK "$INSTDIR\vessel-mcp-old-*.exe"
!macroend
