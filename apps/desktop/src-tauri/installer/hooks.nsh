; NSIS hooks for the hey_buddy per-user installer (S3-1). Included by Tauri's NSIS template.
; Per-user install: $INSTDIR is under %LOCALAPPDATA%\Programs, owned by the installing user, so
; changing its ACL needs NO admin rights.

!macro NSIS_HOOK_PREINSTALL
!macroend

!macro NSIS_HOOK_POSTINSTALL
  ; The agent engine (buddy-agent.exe, next to hey_buddy.exe) runs in an AppContainer, which can
  ; only read/execute files that grant ALL APPLICATION PACKAGES (S-1-15-2-1) read+execute.
  ; Grant (RX) only, never write. Failure is not fatal here: without the ACL the agent
  ; fails closed at runtime (sandbox_failed) and nothing is spawned unsandboxed.
  nsExec::ExecToLog '"$SYSDIR\icacls.exe" "$INSTDIR" /grant "*S-1-15-2-1:(OI)(CI)(RX)"'
  Pop $0
  ${If} $0 != 0
    DetailPrint "Could not grant ALL APPLICATION PACKAGES read access (icacls exit $0). The agent will stay off."
  ${EndIf}
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  ; S3-6 owns data deletion. This hook deliberately deletes NOTHING. Tauri's own uninstaller
  ; page has a "delete the application data" checkbox that is OFF by default; leave it that way.
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
!macroend
