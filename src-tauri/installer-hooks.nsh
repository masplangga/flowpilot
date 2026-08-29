!macro NSIS_HOOK_POSTINSTALL
  CreateShortcut "$SMPROGRAMS\${PRODUCTNAME}.lnk" "$INSTDIR\${MAINBINARYNAME}.exe" "" "$INSTDIR\${MAINBINARYNAME}.exe" 0
  !insertmacro SetLnkAppUserModelId "$SMPROGRAMS\${PRODUCTNAME}.lnk"
  CreateShortcut "$DESKTOP\${PRODUCTNAME}.lnk" "$INSTDIR\${MAINBINARYNAME}.exe" "" "$INSTDIR\${MAINBINARYNAME}.exe" 0
  !insertmacro SetLnkAppUserModelId "$DESKTOP\${PRODUCTNAME}.lnk"
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  ${If} $UpdateMode <> 1
    MessageBox MB_YESNO|MB_ICONQUESTION "Do you also want to remove Flowpilot account data and Google Flow sessions?" IDNO keep_data
    RMDir /r "$LOCALAPPDATA\com.flowpilot.desktop"
    keep_data:
  ${EndIf}
!macroend
