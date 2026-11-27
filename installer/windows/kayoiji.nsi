; Kayoiji の Windows 版のインストーラー。scripts/build-windows-installer.sh が makensis で作る。
;
; DLL は版ごとのフォルダー ($INSTDIR\<版>) に入れる。入力方式の DLL は動いているアプリが読み込んだまま
; 放さないので、同じ場所に上書きできない。新しい版を別のフォルダーに入れて登録し直し、古い版のフォルダーは
; 次のログオンの時に消す。古い版を読み込んだアプリは、そのアプリを起動し直すまで古い版で動く。

Unicode true
!ifndef VERSION
  !error "VERSION is required"
!endif
!ifndef SOURCE
  !error "SOURCE is required"
!endif
!ifndef OUTFILE
  !error "OUTFILE is required"
!endif

!include "MUI2.nsh"
!include "LogicLib.nsh"
!include "x64.nsh"

!define REGKEY "Software\Kayoiji"
!define UNINSTALL_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\Kayoiji"
!define REGSVR32_64 "$WINDIR\Sysnative\regsvr32.exe"
!define REGSVR32_32 "$WINDIR\SysWOW64\regsvr32.exe"

Name "Kayoiji ${VERSION}"
OutFile "${OUTFILE}"
InstallDir "$PROGRAMFILES64\Kayoiji"
RequestExecutionLevel admin
SetCompressor /SOLID lzma

!insertmacro MUI_PAGE_LICENSE "${SOURCE}\LICENSE"
!insertmacro MUI_PAGE_INSTFILES
!define MUI_FINISHPAGE_TEXT "Kayoiji を入れました。$\r$\n$\r$\n「設定 → 時刻と言語 → 言語と地域 → 日本語 → 言語のオプション → キーボードの追加」で Kayoiji を追加してください。入れ替えた場合、動いているアプリは起動し直すまで前の版で動きます。"
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "Japanese"

Function .onInit
  ${IfNot} ${RunningX64}
    MessageBox MB_ICONSTOP "Kayoiji は 64 bit 版の Windows で動きます。"
    Abort
  ${EndIf}
  SetRegView 64
FunctionEnd

Function un.onInit
  SetRegView 64
FunctionEnd

; $0 に、登録した DLL のフォルダーを入れて呼ぶ。
!macro Unregister DIR
  ${If} ${FileExists} "${DIR}\kayoiji-x64.dll"
    ExecWait '"${REGSVR32_64}" /s /u "${DIR}\kayoiji-x64.dll"'
  ${EndIf}
  ${If} ${FileExists} "${DIR}\kayoiji-x86.dll"
    ExecWait '"${REGSVR32_32}" /s /u "${DIR}\kayoiji-x86.dll"'
  ${EndIf}
!macroend

Section "Install"
  ReadRegStr $1 HKLM "${REGKEY}" "InstalledDir"
  ${If} $1 != ""
    !insertmacro Unregister "$1"
  ${EndIf}

  StrCpy $2 "$INSTDIR\${VERSION}"
  SetOutPath "$2"
  File "/oname=kayoiji-x64.dll" "${SOURCE}\x64\kayoiji_windows.dll"
  File "/oname=kayoiji-x86.dll" "${SOURCE}\x86\kayoiji_windows.dll"
  File "${SOURCE}\LICENSE"
  File "${SOURCE}\THIRD-PARTY-NOTICES.txt"
  SetOutPath "$2\data"
  File "${SOURCE}\data\*.*"

  ExecWait '"${REGSVR32_64}" /s "$2\kayoiji-x64.dll"' $0
  ${If} $0 != 0
    MessageBox MB_ICONSTOP "64 bit の DLL を登録できませんでした (regsvr32 の終了コード $0)。"
    Abort
  ${EndIf}
  ExecWait '"${REGSVR32_32}" /s "$2\kayoiji-x86.dll"' $0
  ${If} $0 != 0
    MessageBox MB_ICONSTOP "32 bit の DLL を登録できませんでした (regsvr32 の終了コード $0)。"
    Abort
  ${EndIf}

  ; 前の版のフォルダーは、次にログオンする時に丸ごと消す。前の版を読み込んだアプリは、テキストサービスを
  ; 有効にし直すたびに隣の辞書を開くので、使われていないファイルだけを今消すと、そのアプリで打てなくなる。
  ${If} $1 != ""
  ${AndIf} $1 != $2
    WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\RunOnce" "KayoijiRemove${VERSION}" '"$SYSDIR\cmd.exe" /c rmdir /s /q "$1"'
  ${EndIf}

  WriteRegStr HKLM "${REGKEY}" "InstalledDir" "$2"
  WriteUninstaller "$INSTDIR\uninstall.exe"
  WriteRegStr HKLM "${UNINSTALL_KEY}" "DisplayName" "Kayoiji"
  WriteRegStr HKLM "${UNINSTALL_KEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr HKLM "${UNINSTALL_KEY}" "Publisher" "Cyberneura"
  WriteRegStr HKLM "${UNINSTALL_KEY}" "URLInfoAbout" "https://github.com/cyberneura/kayoiji"
  WriteRegStr HKLM "${UNINSTALL_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKLM "${UNINSTALL_KEY}" "UninstallString" '"$INSTDIR\uninstall.exe"'
  WriteRegDWORD HKLM "${UNINSTALL_KEY}" "NoModify" 1
  WriteRegDWORD HKLM "${UNINSTALL_KEY}" "NoRepair" 1
SectionEnd

Section "Uninstall"
  ReadRegStr $1 HKLM "${REGKEY}" "InstalledDir"
  ${If} $1 != ""
    !insertmacro Unregister "$1"
  ${EndIf}
  ; 利用者のファイル (%APPDATA%\Kayoiji の設定と学習) は残す。
  RMDir /r /REBOOTOK "$INSTDIR"
  DeleteRegKey HKLM "${REGKEY}"
  DeleteRegKey HKLM "${UNINSTALL_KEY}"
SectionEnd
