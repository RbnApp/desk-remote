; Desk-Remote agent installer (DeskRemote-Ajan.exe + Windows service).
; Compiled by scripts/build-installers.ps1 with the NSIS that Tauri downloads. The script passes
; VERSION, EXE_PATH, ICON_PATH, OUT_FILE and the include path of the generated installer-strings.nsh
; (made from locales/en.json + locales/tr.json). Only built-in NSIS plugins are used.
;
; Switches:  /S silent   /D=<dir> target folder (must be last)
;   /CURRENTUSER  files + shortcut + uninstaller for the current user only: no service, no
;                 administrator rights. For sandbox/test runs; a real install is machine-wide
;                 and always registers the service.

Unicode true
!include "MUI2.nsh"
!include "FileFunc.nsh"
!include "LogicLib.nsh"
!include "installer-strings.nsh"

!define APP_ID "DeskRemoteAgent"
!define EXE_NAME "DeskRemote-Ajan.exe"
!define UNINSTALL_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APP_ID}"

Name "Desk-Remote Agent"
OutFile "${OUT_FILE}"
InstallDir "$PROGRAMFILES64\Desk-Remote Agent"
InstallDirRegKey HKLM "Software\${APP_ID}" "InstallDir"
RequestExecutionLevel admin
SetCompressor /SOLID lzma
BrandingText "Desk-Remote ${VERSION}"
VIProductVersion "${VERSION}.0"
VIAddVersionKey "ProductName" "Desk-Remote Agent"
VIAddVersionKey "FileVersion" "${VERSION}"
VIAddVersionKey "FileDescription" "Desk-Remote Agent installer"
VIAddVersionKey "LegalCopyright" "MIT"

!define MUI_ICON "${ICON_PATH}"
!define MUI_UNICON "${ICON_PATH}"
!define MUI_ABORTWARNING
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"
!insertmacro MUI_LANGUAGE "Turkish"

Var UserMode
Var Out

; Text for the running language from the generated strings (single source: locales/*.json).
!macro DrText KEY
  ${If} $LANGUAGE == 1055
    StrCpy $Out "${STR_TR_${KEY}}"
  ${Else}
    StrCpy $Out "${STR_EN_${KEY}}"
  ${EndIf}
!macroend

; Registers Programs and Features under the given hive (HKLM machine-wide, HKCU test mode).
!macro DrRegister HIVE
  WriteRegStr ${HIVE} "Software\${APP_ID}" "InstallDir" "$INSTDIR"
  WriteRegStr ${HIVE} "${UNINSTALL_KEY}" "DisplayName" "Desk-Remote Agent"
  WriteRegStr ${HIVE} "${UNINSTALL_KEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr ${HIVE} "${UNINSTALL_KEY}" "Publisher" "Desk-Remote"
  WriteRegStr ${HIVE} "${UNINSTALL_KEY}" "DisplayIcon" "$INSTDIR\${EXE_NAME},0"
  WriteRegStr ${HIVE} "${UNINSTALL_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr ${HIVE} "${UNINSTALL_KEY}" "UninstallString" '"$INSTDIR\Uninstall.exe"'
  WriteRegStr ${HIVE} "${UNINSTALL_KEY}" "QuietUninstallString" '"$INSTDIR\Uninstall.exe" /S'
  WriteRegDWORD ${HIVE} "${UNINSTALL_KEY}" "NoModify" 1
  WriteRegDWORD ${HIVE} "${UNINSTALL_KEY}" "NoRepair" 1
!macroend

; Removes shortcut, registry entries and files; shared by the failed-install rollback and the uninstaller.
!macro DrRemoveFiles PREFIX
  Function ${PREFIX}RemoveFiles
    Delete "$SMPROGRAMS\Desk-Remote Agent\Desk-Remote Agent.lnk"
    RMDir "$SMPROGRAMS\Desk-Remote Agent"
    DeleteRegKey HKLM "${UNINSTALL_KEY}"
    DeleteRegKey HKCU "${UNINSTALL_KEY}"
    DeleteRegKey HKLM "Software\${APP_ID}"
    DeleteRegKey HKCU "Software\${APP_ID}"
    Delete "$INSTDIR\${EXE_NAME}"
    Delete "$INSTDIR\${EXE_NAME}.bak"
    Delete "$INSTDIR\Uninstall.exe"
    RMDir "$INSTDIR"
  FunctionEnd
!macroend
!insertmacro DrRemoveFiles ""
!insertmacro DrRemoveFiles "un."

; A tray helper outlives a stopped agent for a few seconds and would keep its exe locked: close
; it by its exact image path only. Shared by upgrade and uninstall.
!macro DrCloseStray PREFIX
  Function ${PREFIX}CloseStray
    nsExec::ExecToLog '"$SYSDIR\WindowsPowerShell\v1.0\powershell.exe" -NoProfile -NonInteractive -Command "Get-Process -Name DeskRemote-Ajan -ErrorAction SilentlyContinue | Where-Object { $$_.Path -eq ''$INSTDIR\${EXE_NAME}'' -or $$_.Path -eq ''$INSTDIR\${EXE_NAME}.bak'' } | Stop-Process -Force"'
  FunctionEnd
!macroend
!insertmacro DrCloseStray ""
!insertmacro DrCloseStray "un."

Function .onInit
  StrCpy $UserMode 0
  ${GetParameters} $0
  ${GetOptions} $0 "/CURRENTUSER" $1
  ${IfNot} ${Errors}
    StrCpy $UserMode 1
    ; An explicit /D=<dir> wins; only the untouched default moves to the user profile.
    ${If} $INSTDIR == "$PROGRAMFILES64\Desk-Remote Agent"
      StrCpy $INSTDIR "$LOCALAPPDATA\Desk-Remote Agent"
    ${EndIf}
  ${EndIf}
  ClearErrors
FunctionEnd

; Puts the previous version back after a failed upgrade and starts it again, so the PC is never
; left without an agent. A first install has no backup and is removed instead.
Function RestoreBackup
  ${If} ${FileExists} "$INSTDIR\${EXE_NAME}.bak"
    Delete "$INSTDIR\${EXE_NAME}"
    Rename "$INSTDIR\${EXE_NAME}.bak" "$INSTDIR\${EXE_NAME}"
    ${If} $UserMode == 0
      nsExec::ExecToLog '"$INSTDIR\${EXE_NAME}" install'
    ${EndIf}
    !insertmacro DrText update_restored
    DetailPrint "$Out"
  ${Else}
    Call RemoveFiles
  ${EndIf}
FunctionEnd

Section "Install"
  ${If} $UserMode == 0
    SetShellVarContext all
  ${Else}
    SetShellVarContext current
  ${EndIf}

  ; An upgrade: the running copy must not hold the files. End it the same way the uninstaller
  ; does, then keep the old exe as a backup until the new one is in place and registered.
  ${If} ${FileExists} "$INSTDIR\${EXE_NAME}"
    !insertmacro DrText stop_tray
    DetailPrint "$Out"
    nsExec::ExecToLog '"$INSTDIR\${EXE_NAME}" stop'
    Sleep 1000
    Call CloseStray
    Delete "$INSTDIR\${EXE_NAME}.bak"
    ClearErrors
    Rename "$INSTDIR\${EXE_NAME}" "$INSTDIR\${EXE_NAME}.bak"
    ${If} ${Errors}
      ; Still locked: nothing was changed, so just start the old agent again.
      ${If} $UserMode == 0
        nsExec::ExecToLog '"$INSTDIR\${EXE_NAME}" install'
      ${EndIf}
      !insertmacro DrText update_restored
      DetailPrint "$Out"
      SetErrorLevel 3
      Abort
    ${EndIf}
  ${EndIf}

  SetOutPath "$INSTDIR"
  ClearErrors
  File "/oname=${EXE_NAME}" "${EXE_PATH}"
  ${If} ${Errors}
    Call RestoreBackup
    SetErrorLevel 3
    Abort
  ${EndIf}
  CreateDirectory "$SMPROGRAMS\Desk-Remote Agent"
  CreateShortcut "$SMPROGRAMS\Desk-Remote Agent\Desk-Remote Agent.lnk" "$INSTDIR\${EXE_NAME}" "" "$INSTDIR\${EXE_NAME}" 0
  WriteUninstaller "$INSTDIR\Uninstall.exe"

  ${If} $UserMode == 0
    !insertmacro DrRegister HKLM
    !insertmacro DrText service_install
    DetailPrint "$Out"
    ; The exe refuses on a panel PC (role guard) and needs administrator rights; its exit
    ; code and message decide whether the new version stays. `install` also upgrades an
    ; existing service in place and starts it.
    nsExec::ExecToStack '"$INSTDIR\${EXE_NAME}" install'
    Pop $0
    Pop $1
    ${If} $0 != 0
      DetailPrint "$1"
      !insertmacro DrText service_failed
      ${IfNot} ${Silent}
        MessageBox MB_OK|MB_ICONSTOP "$Out$\r$\n$\r$\n$1"
      ${EndIf}
      Call RestoreBackup
      SetErrorLevel 2
      Abort
    ${EndIf}
  ${Else}
    !insertmacro DrRegister HKCU
  ${EndIf}
  ; The upgrade went through: the backup is no longer needed. The stopped agent may still be
  ; winding down and holding its exe, so try for a few seconds.
  StrCpy $R0 0
  ${Do}
    ClearErrors
    Delete "$INSTDIR\${EXE_NAME}.bak"
    ${IfNot} ${Errors}
    ${OrIf} $R0 >= 10
      ${Break}
    ${EndIf}
    IntOp $R0 $R0 + 1
    Sleep 1000
  ${Loop}
SectionEnd

Function un.onInit
  ; Machine-wide install -> all-users context, else the current user's.
  ReadRegStr $0 HKLM "Software\${APP_ID}" "InstallDir"
  ${If} $0 != ""
    StrCpy $UserMode 0
    SetShellVarContext all
  ${Else}
    StrCpy $UserMode 1
    SetShellVarContext current
  ${EndIf}
FunctionEnd

Section "Uninstall"
  ${If} $UserMode == 0
    !insertmacro DrText service_remove
    DetailPrint "$Out"
    ; 1) ends a console-mode agent or stops the service (its tray helper goes with it);
    ; 2) deletes the service. Both best effort so a half-installed agent can still be removed.
    nsExec::ExecToLog '"$INSTDIR\${EXE_NAME}" stop'
    nsExec::ExecToLog '"$INSTDIR\${EXE_NAME}" uninstall'
    Call un.CloseStray
    Sleep 500
  ${EndIf}
  Call un.RemoveFiles
SectionEnd
