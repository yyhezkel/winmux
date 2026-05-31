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
;
; Uses WordFunc's WordReplace as both a presence check and a removal
; primitive — when `$out == $in` after a WordReplace, the substring
; wasn't found. Avoids StrFunc, which needs a different activation
; pattern that bit us in 48-B's first attempt.

!include "WordFunc.nsh"
!insertmacro WordReplace
!insertmacro un.WordReplace

!macro NSIS_HOOK_POSTINSTALL
  Push $0
  Push $1
  ReadRegStr $0 HKCU "Environment" "Path"
  ; Presence check: WordReplace returns the original string unchanged
  ; when the search substring isn't found. So $1 == $0 means INSTDIR
  ; isn't currently on PATH.
  ${WordReplace} "$0" "$INSTDIR" "" "+" $1
  ${If} $1 == $0
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
    ; Try the two common forms ";$INSTDIR" and "$INSTDIR;" then bare
    ; "$INSTDIR". Each ${un.WordReplace} no-ops if its substring isn't
    ; found, so chaining them is safe.
    ${un.WordReplace} "$0" ";$INSTDIR" "" "+" $1
    ${un.WordReplace} "$1" "$INSTDIR;" "" "+" $2
    ${un.WordReplace} "$2" "$INSTDIR" "" "+" $1
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
