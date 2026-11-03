; Polskie Studio Map – instalator Windows (NSIS 3, Unicode). Plik zapisany jako UTF-8 z BOM.
; makensis /DVERSION=0.1.0 /DEXE=..\target\release\psm.exe /DOUT=..\release\PSM-Setup-0.1.0.exe build\psm.nsi
;
; Instalacja dla wszystkich użytkowników (Program Files, okno UAC). Tryb cichy: /S; po instalacji
; w trybie cichym program uruchamia się z opcją --force-run (tak wywołuje go aktualizator).

Unicode true
ManifestDPIAware true
SetCompressor /SOLID lzma
RequestExecutionLevel admin

!ifndef VERSION
  !define VERSION "0.0.0"
!endif
!ifndef EXE
  !define EXE "..\target\release\psm.exe"
!endif
!ifndef OUT
  !define OUT "..\release\PSM-Setup-${VERSION}.exe"
!endif

!define NAME "Polskie Studio Map"
!define UNKEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\PSM"

!include "MUI2.nsh"
!include "FileFunc.nsh"
!include "LogicLib.nsh"
!include "x64.nsh"

Name "${NAME}"
OutFile "${OUT}"
InstallDir "$PROGRAMFILES64\${NAME}"
InstallDirRegKey HKLM "${UNKEY}" "InstallLocation"
BrandingText "Polskie Studio Map ${VERSION} · redzikowski.com"

VIProductVersion "${VERSION}.0"
VIAddVersionKey /LANG=1045 "ProductName" "${NAME}"
VIAddVersionKey /LANG=1045 "FileDescription" "Polskie Studio Map – atlas warstw mapowych Polski 3D (instalator)"
VIAddVersionKey /LANG=1045 "CompanyName" "BlackSingularities"
VIAddVersionKey /LANG=1045 "LegalCopyright" "© 2026 Adam Rędzikowski"
VIAddVersionKey /LANG=1045 "FileVersion" "${VERSION}"
VIAddVersionKey /LANG=1045 "ProductVersion" "${VERSION}"

!define MUI_ICON "icon.ico"
!define MUI_UNICON "icon.ico"
!define MUI_WELCOMEFINISHPAGE_BITMAP "installerSidebar.bmp"
!define MUI_UNWELCOMEFINISHPAGE_BITMAP "uninstallerSidebar.bmp"
!define MUI_ABORTWARNING
!define MUI_FINISHPAGE_RUN
!define MUI_FINISHPAGE_RUN_TEXT "Uruchom Polskie Studio Map"
!define MUI_FINISHPAGE_RUN_FUNCTION RunApp

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "Polish"

; uruchomienie bez uprawnień administratora (przez Eksploratora, jako zwykły użytkownik)
Function RunApp
  Exec '"$WINDIR\explorer.exe" "$INSTDIR\psm.exe"'
FunctionEnd

Function .onInit
  ${IfNot} ${RunningX64}
    MessageBox MB_ICONSTOP "Polskie Studio Map wymaga 64-bitowego systemu Windows 10 lub 11." /SD IDOK
    Abort
  ${EndIf}
  SetRegView 64
  ; dotychczasowy folder
  ReadRegStr $0 HKLM "${UNKEY}" "InstallLocation"
  ${If} $0 != ""
    StrCpy $INSTDIR $0
  ${EndIf}
FunctionEnd

; program musi być zamknięty, zanim podmienimy plik
!macro CloseApp
  StrCpy $1 0
  ${Do}
    ClearErrors
    Delete "$INSTDIR\psm.exe"
    ${IfNot} ${Errors}
      ${Break}
    ${EndIf}
    IntOp $1 $1 + 1
    ${If} $1 = 12
      nsExec::Exec 'taskkill /F /IM psm.exe'
      Sleep 800
    ${EndIf}
    ${If} $1 > 20
      MessageBox MB_RETRYCANCEL|MB_ICONEXCLAMATION "Zamknij program Polskie Studio Map, żeby kontynuować." /SD IDCANCEL IDRETRY psm_retry
      Abort
      psm_retry:
      StrCpy $1 0
    ${EndIf}
    Sleep 500
  ${Loop}
!macroend

Section "Polskie Studio Map" SecMain
  SetShellVarContext all
  SetRegView 64
  SetOutPath "$INSTDIR"
  ${If} ${FileExists} "$INSTDIR\psm.exe"
    !insertmacro CloseApp
  ${EndIf}

  File "/oname=psm.exe" "${EXE}"
  WriteUninstaller "$INSTDIR\Odinstaluj.exe"

  CreateShortCut "$SMPROGRAMS\${NAME}.lnk" "$INSTDIR\psm.exe" "" "$INSTDIR\psm.exe" 0
  CreateShortCut "$DESKTOP\${NAME}.lnk" "$INSTDIR\psm.exe" "" "$INSTDIR\psm.exe" 0

  ; linki psm://widok?… otwierają widok w programie
  WriteRegStr HKCR "psm" "" "URL:Polskie Studio Map – widok mapy"
  WriteRegStr HKCR "psm" "URL Protocol" ""
  WriteRegStr HKCR "psm\DefaultIcon" "" '"$INSTDIR\psm.exe",0'
  WriteRegStr HKCR "psm\shell\open\command" "" '"$INSTDIR\psm.exe" "%1"'

  WriteRegStr HKLM "${UNKEY}" "DisplayName" "Polskie Studio Map – atlas warstw mapowych Polski 3D"
  WriteRegStr HKLM "${UNKEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr HKLM "${UNKEY}" "DisplayIcon" "$INSTDIR\psm.exe"
  WriteRegStr HKLM "${UNKEY}" "Publisher" "BlackSingularities"
  WriteRegStr HKLM "${UNKEY}" "URLInfoAbout" "https://redzikowski.com/portfolio/polskie-studio-map"
  WriteRegStr HKLM "${UNKEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKLM "${UNKEY}" "UninstallString" '"$INSTDIR\Odinstaluj.exe"'
  WriteRegStr HKLM "${UNKEY}" "QuietUninstallString" '"$INSTDIR\Odinstaluj.exe" /S'
  WriteRegDWORD HKLM "${UNKEY}" "NoModify" 1
  WriteRegDWORD HKLM "${UNKEY}" "NoRepair" 1
  ${GetSize} "$INSTDIR" "/S=0K" $0 $1 $2
  IntFmt $0 "0x%08X" $0
  WriteRegDWORD HKLM "${UNKEY}" "EstimatedSize" "$0"
SectionEnd

Function .onInstSuccess
  ${If} ${Silent}
    ${GetParameters} $R0
    ClearErrors
    ${GetOptions} $R0 "--force-run" $R1
    ${IfNot} ${Errors}
      Call RunApp
    ${EndIf}
  ${EndIf}
FunctionEnd

Section "Uninstall"
  SetShellVarContext all
  SetRegView 64
  nsExec::Exec 'taskkill /F /IM psm.exe'
  Delete "$INSTDIR\psm.exe"
  Delete "$INSTDIR\Odinstaluj.exe"
  RMDir "$INSTDIR"
  Delete "$SMPROGRAMS\${NAME}.lnk"
  Delete "$DESKTOP\${NAME}.lnk"
  DeleteRegKey HKLM "${UNKEY}"
  DeleteRegKey HKCR "psm"

  ; przy aktualizacji (--updated) albo w trybie cichym dane użytkownika zostają nietknięte
  ${GetParameters} $R0
  ClearErrors
  ${GetOptions} $R0 "--updated" $R1
  ${If} ${Errors}
  ${AndIfNot} ${Silent}
    MessageBox MB_YESNO|MB_ICONQUESTION|MB_DEFBUTTON2 "Usunąć także pobrane dane map (kafle terenu, szlaki, zapisane obszary) i ustawienia programu?$\r$\n$\r$\nWybierz „Nie”, aby zachować je na wypadek ponownej instalacji." IDNO keep
      SetShellVarContext current
      ; folder danych wybrany przez użytkownika (zapisany przez program w datadir.txt, UTF-16 LE)
      ClearErrors
      FileOpen $0 "$APPDATA\PSM\datadir.txt" r
      ${IfNot} ${Errors}
        FileReadUTF16LE $0 $1
        FileClose $0
        ; usuwany jest tylko własny podfolder programu – nigdy wskazany katalog nadrzędny
        StrCpy $2 $1 8 -8
        ${If} $2 == "PSM-dane"
          RMDir /r "$1"
        ${EndIf}
      ${EndIf}
      RMDir /r "$LOCALAPPDATA\PSM"
      RMDir /r "$APPDATA\PSM"
      SetShellVarContext all
    keep:
  ${EndIf}
SectionEnd
