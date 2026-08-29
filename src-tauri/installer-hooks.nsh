!macro NSIS_HOOK_PREUNINSTALL
  ${If} $UpdateMode <> 1
    MessageBox MB_YESNO|MB_ICONQUESTION "Do you also want to remove Flowpilot account data and Google Flow sessions?" IDNO keep_data
    RMDir /r "$LOCALAPPDATA\com.flowpilot.desktop"
    keep_data:
  ${EndIf}
!macroend
