; NGD_KF NSIS 설치 후크(2026-10-05) - Windows Defender 실시간 검사 예외 등록.
;
; 배경: 서명 안 된(Authenticode 인증서 없는) 새 exe를 설치하면 Windows
; Defender가 _internal 안 12,000개 이상의 파일을 하나씩 실시간 검사하면서
; 첫 실행 예열이 수십 분까지 걸리는 문제를 실측으로 확인함(2026-10-05).
; 설치 폴더를 Defender 제외 목록에 등록해두면 이 문제가 사라진다.
;
; Add-MpPreference는 관리자 권한이 필요해서, 기본 설치(currentUser,
; 무권한)는 그대로 두고 이 단계만 별도로 "runas"로 관리자 승인을 한
; 번 요청한다 - 사용자가 거부해도 설치 자체는 이미 끝난 뒤라 앱은
; 정상 동작한다(그냥 첫 예열이 느릴 뿐).

!macro NSIS_HOOK_POSTINSTALL
  DetailPrint "Windows Defender 검사 예외 등록 준비 중..."
  FileOpen $0 "$INSTDIR\_add_defender_exclusion.ps1" w
  FileWrite $0 "Add-MpPreference -ExclusionPath '$INSTDIR' -ErrorAction SilentlyContinue$\r$\n"
  FileClose $0
  DetailPrint "Windows Defender 예외 등록 시도 중 (관리자 권한 확인창이 뜰 수 있습니다 - 거부해도 설치는 계속됩니다)..."
  ExecShell "runas" "powershell.exe" '-NoProfile -WindowStyle Hidden -ExecutionPolicy Bypass -File "$INSTDIR\_add_defender_exclusion.ps1"'
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  DetailPrint "Windows Defender 검사 예외 해제 시도 중 (관리자 권한 확인창이 뜰 수 있습니다)..."
  FileOpen $0 "$INSTDIR\_remove_defender_exclusion.ps1" w
  FileWrite $0 "Remove-MpPreference -ExclusionPath '$INSTDIR' -ErrorAction SilentlyContinue$\r$\n"
  FileClose $0
  ExecShell "runas" "powershell.exe" '-NoProfile -WindowStyle Hidden -ExecutionPolicy Bypass -File "$INSTDIR\_remove_defender_exclusion.ps1"'
!macroend
