; Phase 48-B: NSIS installer hooks for winmux.
;
; Adds the install directory to the user's PATH on install and removes
; it on uninstall. User-level (HKCU\Environment) — no admin elevation
; required. Broadcasts WM_SETTINGCHANGE so newly-spawned shells pick
; up the change without a reboot. Existing shells still need a restart
; to see the new PATH (Windows limitation, not ours).
;
; Wired via tauri.conf.json bundle.windows.nsis.installerHooks. Tauri
; 2 invokes these macros at the documented install/uninstall lifecycle
; points.

!include "StrFunc.nsh"
!include "WordFunc.nsh"
${StrLoc}
${UnStrLoc}

!macro NSIS_HOOK_POSTINSTALL
  Push $0
  Push $1
  ReadRegStr $0 HKCU "Environment" "Path"
  ; Skip if our INSTDIR is already on the PATH (idempotent).
  ${StrLoc} $1 "$0" "$INSTDIR" ">"
  ${If} $1 == ""
    ${If} $0 == ""
      WriteRegExpandStr HKCU "Environment" "Path" "$INSTDIR"
    ${Else}
      WriteRegExpandStr HKCU "Environment" "Path" "$0;$INSTDIR"
    ${EndIf}
    ; Tell other processes the environment has changed. 5000ms timeout
    ; so a hung Explorer doesn't block the installer.
    System::Call 'user32::SendMessageTimeoutW(p 0xFFFF, i 0x1A, p 0, w "Environment", i 0, i 5000, *p .r1)'
    DetailPrint "Added $INSTDIR to user PATH"
  ${Else}
    DetailPrint "$INSTDIR already in user PATH — skipping"
  ${EndIf}
  Pop $1
  Pop $0
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  Push $0
  Push $1
  Push $2
  ReadRegStr $0 HKCU "Environment" "Path"
  ${If} $0 != ""
    ; Best-effort removal: try the two common forms ";$INSTDIR" and
    ; "$INSTDIR;" then bare "$INSTDIR". Each ${WordReplace} no-ops if
    ; the substring isn't found.
    ${WordReplace} "$0" ";$INSTDIR" "" "+" $1
    ${WordReplace} "$1" "$INSTDIR;" "" "+" $2
    ${WordReplace} "$2" "$INSTDIR" "" "+" $1
    ${If} $1 != $0
      WriteRegExpandStr HKCU "Environment" "Path" "$1"
      System::Call 'user32::SendMessageTimeoutW(p 0xFFFF, i 0x1A, p 0, w "Environment", i 0, i 5000, *p .r2)'
      DetailPrint "Removed $INSTDIR from user PATH"
    ${EndIf}
  ${EndIf}
  Pop $2
  Pop $1
  Pop $0
!macroend
