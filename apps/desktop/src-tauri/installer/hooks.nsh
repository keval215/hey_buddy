; NSIS hooks for the hey_buddy per-user installer (S3-1). Included by Tauri's NSIS template.
; Per-user install: $INSTDIR is user-selectable but normally under %LOCALAPPDATA%\Programs, owned
; by the installing user, so changing its ACL needs NO admin rights.

!macro NSIS_HOOK_PREINSTALL
!macroend

!macro NSIS_HOOK_POSTINSTALL
  ; The agent engine (buddy-agent.exe, installed beside the Tauri-produced main app exe, likely
  ; desktop.exe; verify on Windows) runs in an AppContainer, which can only read/execute files that
  ; grant ALL APPLICATION PACKAGES (S-1-15-2-1) read+execute. buddy-agent is a static Rust exe with
  ; no DLLs of ours beside it, so the narrowest grant is: (RX) on the install dir itself (NOT
  ; inherited: traverse only) and (RX) on buddy-agent.exe only. Never write, never (OI)(CI).
  ;
  ; Fail closed: if $INSTDIR is a drive root or a well-known user folder (the user picked a custom
  ; dir) we skip the grant and warn. Without the ACL the agent fails closed at runtime
  ; (sandbox_failed); nothing is spawned unsandboxed.
  StrCpy $1 "$INSTDIR"
  StrCpy $2 "$1" 1 -1
  ${If} $2 == "\"
    StrCpy $1 "$1" -1
  ${EndIf}
  StrLen $2 "$1"
  StrCpy $3 "0"
  ${If} $2 <= 2
    StrCpy $3 "1"
  ${EndIf}
  ${If} $1 == "$PROFILE"
  ${OrIf} $1 == "$DOCUMENTS"
  ${OrIf} $1 == "$DESKTOP"
  ${OrIf} $1 == "$LOCALAPPDATA"
  ${OrIf} $1 == "$APPDATA"
  ${OrIf} $1 == "$PROGRAMFILES"
  ${OrIf} $1 == "$PROGRAMFILES64"
    StrCpy $3 "1"
  ${EndIf}
  ${If} $3 S== "1"
    DetailPrint "Install dir $INSTDIR is a drive root or a user folder: skipping the AppContainer ACL grant. The agent sandbox will be unavailable."
    MessageBox MB_OK|MB_ICONEXCLAMATION "hey_buddy was installed into $INSTDIR, a drive root or a general user folder. For safety the agent sandbox permission was NOT granted, so the agent engine will stay off. Reinstall into a dedicated folder (the default) to enable it."
  ${Else}
    nsExec::ExecToLog '"$SYSDIR\icacls.exe" "$INSTDIR" /grant "*S-1-15-2-1:(RX)"'
    Pop $0
    ${If} $0 S!= "0"
      DetailPrint "Could not grant ALL APPLICATION PACKAGES access to the install dir (exit code $0). The agent will stay off."
    ${EndIf}
    nsExec::ExecToLog '"$SYSDIR\icacls.exe" "$INSTDIR\buddy-agent.exe" /grant "*S-1-15-2-1:(RX)"'
    Pop $0
    ${If} $0 S!= "0"
      DetailPrint "Could not grant ALL APPLICATION PACKAGES access to buddy-agent.exe (exit code $0). The agent will stay off."
    ${EndIf}
  ${EndIf}
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  ; S3-6 owns data deletion. This hook deliberately removes NOTHING. Tauri's own uninstaller
  ; page has a "remove the application data" checkbox that is OFF by default; leave it that way.
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
!macroend
