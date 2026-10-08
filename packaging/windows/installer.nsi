Unicode true
!include "MUI2.nsh"
Name "Fastdistord 0.01"
OutFile "..\..\dist\Fastdistord-0.01-windows-x64-setup.exe"
InstallDir "$LOCALAPPDATA\Programs\Fastdistord"
RequestExecutionLevel user
SetCompressor /SOLID lzma
VIProductVersion "0.0.1.0"
VIAddVersionKey "ProductName" "Fastdistord"
VIAddVersionKey "ProductVersion" "0.01"
VIAddVersionKey "FileVersion" "0.0.1.0"
VIAddVersionKey "FileDescription" "Fastdistord installer"
VIAddVersionKey "LegalCopyright" "MIT licensed"
!insertmacro MUI_PAGE_LICENSE "..\..\LICENSE"
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"
Section "Fastdistord"
  SetShellVarContext current
  SetOutPath "$INSTDIR"
  File "..\..\target\release\fastdistord.exe"
  File /oname=LICENSE.txt "..\..\LICENSE"
  SetOutPath "$INSTDIR\third-party"
  File /r "..\..\dist\third-party\*"
  SetOutPath "$INSTDIR"
  WriteUninstaller "$INSTDIR\Uninstall.exe"
  CreateDirectory "$SMPROGRAMS\Fastdistord"
  CreateShortcut "$SMPROGRAMS\Fastdistord\Fastdistord.lnk" "$INSTDIR\fastdistord.exe"
  CreateShortcut "$SMPROGRAMS\Fastdistord\Uninstall.lnk" "$INSTDIR\Uninstall.exe"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Fastdistord" "DisplayName" "Fastdistord"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Fastdistord" "DisplayVersion" "0.01"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Fastdistord" "UninstallString" '$\"$INSTDIR\Uninstall.exe$\"'
  WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Fastdistord" "NoModify" 1
  WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Fastdistord" "NoRepair" 1
SectionEnd
Section "Uninstall"
  SetShellVarContext current
  Delete "$INSTDIR\fastdistord.exe"
  Delete "$INSTDIR\LICENSE.txt"
  Delete "$INSTDIR\Uninstall.exe"
  RMDir /r "$INSTDIR\third-party"
  RMDir "$INSTDIR"
  Delete "$SMPROGRAMS\Fastdistord\Fastdistord.lnk"
  Delete "$SMPROGRAMS\Fastdistord\Uninstall.lnk"
  RMDir "$SMPROGRAMS\Fastdistord"
  DeleteRegKey HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Fastdistord"
SectionEnd
