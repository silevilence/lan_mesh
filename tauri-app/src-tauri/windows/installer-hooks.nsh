; Tauri intentionally skips shortcut recreation during /UPDATE installs.
; Preserve the user's shortcut choices, recreate the existing shortcuts after
; the executable is replaced, then tell Explorer to invalidate its icon cache.

Var LanMeshHadDesktopShortcut
Var LanMeshHadStartMenuShortcut

!macro NSIS_HOOK_PREINSTALL
  StrCpy $LanMeshHadDesktopShortcut 0
  StrCpy $LanMeshHadStartMenuShortcut 0

  ${If} $UpdateMode = 1
    IfFileExists "$DESKTOP\${PRODUCTNAME}.lnk" 0 +2
      StrCpy $LanMeshHadDesktopShortcut 1

    !if "${STARTMENUFOLDER}" != ""
      IfFileExists "$SMPROGRAMS\${STARTMENUFOLDER}\${PRODUCTNAME}.lnk" 0 +2
        StrCpy $LanMeshHadStartMenuShortcut 1
    !else
      IfFileExists "$SMPROGRAMS\${PRODUCTNAME}.lnk" 0 +2
        StrCpy $LanMeshHadStartMenuShortcut 1
    !endif
  ${EndIf}
!macroend

!macro NSIS_HOOK_POSTINSTALL
  ${If} $UpdateMode = 1
    ${If} $LanMeshHadDesktopShortcut = 1
      Delete "$DESKTOP\${PRODUCTNAME}.lnk"
      CreateShortcut "$DESKTOP\${PRODUCTNAME}.lnk" "$INSTDIR\${MAINBINARYNAME}.exe"
      !insertmacro SetLnkAppUserModelId "$DESKTOP\${PRODUCTNAME}.lnk"
    ${EndIf}

    ${If} $LanMeshHadStartMenuShortcut = 1
      !if "${STARTMENUFOLDER}" != ""
        Delete "$SMPROGRAMS\${STARTMENUFOLDER}\${PRODUCTNAME}.lnk"
        CreateShortcut "$SMPROGRAMS\${STARTMENUFOLDER}\${PRODUCTNAME}.lnk" "$INSTDIR\${MAINBINARYNAME}.exe"
        !insertmacro SetLnkAppUserModelId "$SMPROGRAMS\${STARTMENUFOLDER}\${PRODUCTNAME}.lnk"
      !else
        Delete "$SMPROGRAMS\${PRODUCTNAME}.lnk"
        CreateShortcut "$SMPROGRAMS\${PRODUCTNAME}.lnk" "$INSTDIR\${MAINBINARYNAME}.exe"
        !insertmacro SetLnkAppUserModelId "$SMPROGRAMS\${PRODUCTNAME}.lnk"
      !endif
    ${EndIf}
  ${EndIf}

  ; SHCNE_ASSOCCHANGED + SHCNF_FLUSH invalidates the Shell icon cache.
  System::Call 'shell32::SHChangeNotify(i 0x08000000, i 0x00001000, p 0, p 0)'
!macroend
