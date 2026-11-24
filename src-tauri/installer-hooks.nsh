!macro NSIS_HOOK_POSTINSTALL
  ReadRegStr $0 HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "Vela"
  ${If} $0 != ""
    WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "Vela" '$\"$INSTDIR\vela.exe$\" --vela-login-startup'
  ${EndIf}
  WriteRegStr HKCU "Software\Classes\Vela.CCSwitch" "" "Vela CC Switch Link"
  WriteRegStr HKCU "Software\Classes\Vela.CCSwitch" "URL Protocol" ""
  WriteRegStr HKCU "Software\Classes\Vela.CCSwitch\shell\open\command" "" '$\"$INSTDIR\vela.exe$\" $\"%1$\"'
  WriteRegStr HKCU "Software\Vela\Capabilities" "ApplicationName" "Vela"
  WriteRegStr HKCU "Software\Vela\Capabilities" "ApplicationDescription" "CC Switch links"
  WriteRegStr HKCU "Software\Vela\Capabilities\URLAssociations" "ccswitch" "Vela.CCSwitch"
  WriteRegStr HKCU "Software\RegisteredApplications" "Vela Link Vela" "Software\Vela\Capabilities"
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "Vela"
  DeleteRegKey HKCU "Software\Classes\Vela.CCSwitch"
  DeleteRegKey HKCU "Software\Vela\Capabilities"
  DeleteRegKey /ifempty HKCU "Software\Vela"
  DeleteRegValue HKCU "Software\RegisteredApplications" "Vela Link Vela"
  ReadRegStr $0 HKCU "Software\Classes\ccswitch\shell\open\command" ""
  ${If} $0 == '$\"$INSTDIR\vela.exe$\" $\"%1$\"'
    DeleteRegKey HKCU "Software\Classes\ccswitch"
  ${EndIf}
!macroend
