// NGD_KF Tauri 셸(2026-09-29, 2단계) - Flask 백엔드(sidecar)를 자동으로
// 띄우고, 포트가 응답할 때까지 로딩 화면을 보여준 뒤 실제 화면(localhost:5000)
// 으로 넘어간다. 앱 종료 시에는 (1)Flask 자신에게 /shutdown을 호출해 스스로
// 완전히 종료하게 하고, (2)그와 별개로 PID 기준 taskkill /T로 프로세스
// 트리를 통째로 정리하는 안전망을 반드시 함께 건다 - PyInstaller onedir
// exe가 부트로더/실제 프로세스를 분리해 띄우는 빌드에서는 Tauri의
// CommandChild.kill()만으로 고아 프로세스가 남을 수 있다는 알려진 문제
// (tauri-apps/tauri#11686) 때문에 둘 다 건다(어느 한쪽만 믿지 않음).

use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;
use std::sync::Mutex;
use std::time::Duration;
use tauri::webview::DownloadEvent;
use tauri::{Manager, WebviewWindowBuilder, WindowEvent};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons};
use tauri_plugin_shell::process::CommandChild;
use tauri_plugin_shell::ShellExt;
use tauri_plugin_updater::UpdaterExt;

const BACKEND_URL: &str = "http://127.0.0.1:5000";
// 자동 업데이트 재확인 주기(2026-09-30, GitHub Release 연동) - 앱을 켠
// 상태로 오래 두고 쓰는 경우(교사가 하루 종일 켜둘 수 있음)를 대비해
// 실행 시점 1회 + 이 주기로 반복 확인한다.
const UPDATE_CHECK_INTERVAL: Duration = Duration::from_secs(4 * 60 * 60);

struct SidecarState(Mutex<Option<CommandChild>>);

fn wait_for_backend_and_navigate(app_handle: tauri::AppHandle) {
    std::thread::spawn(move || {
        let ping_url = format!("{}/", BACKEND_URL);
        // 캐시 예열까지 포함해 최악 케이스도 커버하도록 넉넉히 기다린다
        // (실측 최대 약 20초, 여유를 둬서 최대 5분/1000회 x 300ms).
        for _ in 0..1000 {
            if ureq::get(&ping_url)
                .timeout(Duration::from_secs(2))
                .call()
                .is_ok()
            {
                if let Some(window) = app_handle.get_webview_window("main") {
                    if let Ok(url) = BACKEND_URL.parse() {
                        let _ = window.navigate(url);
                        let _ = window.set_title("NGD_KF");
                    }
                }
                return;
            }
            std::thread::sleep(Duration::from_millis(300));
        }
        eprintln!("[NGD_KF] 백엔드가 시간 안에 준비되지 않았습니다 - 로딩 화면에 머무릅니다.");
    });
}

/// 앱 종료 시 sidecar를 확실하게 완전 종료시킨다(그래스풀 + 강제, 둘 다).
fn shutdown_sidecar(child_pid: Option<u32>) {
    // 1) 그래스풀 - Flask 프로세스 자신이 os._exit(0)로 스스로 죽게 함
    //    (부트로더/자식 프로세스 구조와 무관하게 항상 "실제로 요청을
    //    처리 중인 그 프로세스"를 죽이므로 가장 확실함).
    let _ = ureq::post(&format!("{}/shutdown", BACKEND_URL))
        .timeout(Duration::from_secs(2))
        .call();
    std::thread::sleep(Duration::from_millis(800));

    // 2) 강제 안전망 - PID 기준으로 프로세스 트리를 통째로 정리.
    //    이미 위에서 정상 종료됐으면 taskkill은 그냥 "대상 없음"으로
    //    조용히 실패할 뿐이라 안전하게 매번 호출해도 된다.
    if let Some(pid) = child_pid {
        let _ = StdCommand::new("taskkill")
            .args(["/F", "/T", "/PID", &pid.to_string()])
            .creation_flags(0x08000000) // CREATE_NO_WINDOW - 콘솔 창 깜빡임 방지
            .output();
    }
}

#[cfg(windows)]
trait CommandExt2 {
    fn creation_flags(&mut self, flags: u32) -> &mut Self;
}
#[cfg(windows)]
impl CommandExt2 for StdCommand {
    fn creation_flags(&mut self, flags: u32) -> &mut Self {
        use std::os::windows::process::CommandExt;
        CommandExt::creation_flags(self, flags)
    }
}
#[cfg(not(windows))]
trait CommandExt2 {
    fn creation_flags(&mut self, _flags: u32) -> &mut Self;
}
#[cfg(not(windows))]
impl CommandExt2 for StdCommand {
    fn creation_flags(&mut self, _flags: u32) -> &mut Self {
        self
    }
}

/// GitHub Release(latest.json)를 확인해서 새 버전이 있으면 물어보고,
/// 승낙하면 내려받아 설치한 뒤 앱을 재시작한다(2026-09-30). DB(데이터
/// 폴더)는 이 과정에서 전혀 열거나 옮기지 않는다 - 업데이터가 건드리는
/// 대상은 프로그램 설치 폴더(exe/sidecar/리소스)뿐이고, 데이터 폴더
/// 경로는 %APPDATA%\NGD_KF\data_location.json에 별도로 저장돼 있어
/// 프로그램 파일 교체와 물리적으로 분리되어 있다(ngd_paths.py 참고).
async fn check_for_update_and_prompt(app_handle: &tauri::AppHandle) {
    let updater = match app_handle.updater() {
        Ok(u) => u,
        Err(e) => {
            eprintln!("[NGD_KF] updater 초기화 실패(무시): {e}");
            return;
        }
    };
    match updater.check().await {
        Ok(Some(update)) => {
            let version = update.version.clone();
            let notes = update.body.clone().unwrap_or_default();
            println!("[NGD_KF] 새 버전 발견: {version}");
            let should_install = app_handle
                .dialog()
                .message(format!(
                    "새 버전 {version}이(가) 있습니다.\n\n{notes}\n\n지금 업데이트할까요?\n(설치를 위해 프로그램이 잠시 종료됩니다 - 종료 후 아이콘을 다시 눌러 실행해주세요)"
                ))
                .title("NGD_KF 업데이트 확인")
                .buttons(MessageDialogButtons::YesNo)
                .blocking_show();
            if !should_install {
                println!("[NGD_KF] 사용자가 업데이트를 미뤘습니다.");
                return;
            }
            // Windows에서는 설치 프로그램이 실행되는 순간 이 프로세스가
            // 강제 종료된다(Tauri 공식 문서: "the application is
            // automatically exited when the install step is executed
            // due to a limitation of Windows installers") - 그래서 아래
            // app_handle.restart() 줄에 아예 도달하지 못할 수 있고,
            // on_window_event의 CloseRequested 핸들러도 이 경로에서는
            // 안 탄다(사용자가 창을 닫은 게 아니라 설치 프로그램이 강제
            // 종료시키는 것이라 WM_CLOSE가 안 옴). 그 결과 sidecar가
            // 고아로 남는 것을 실제 업데이트 테스트로 확인함(2026-09-30)
            // - 그래서 다운로드/설치를 시작하기 "전에" 우리가 먼저
            // sidecar를 확실히 내린다.
            let sidecar_pid = app_handle
                .state::<SidecarState>()
                .0
                .lock()
                .unwrap()
                .take()
                .map(|c| c.pid());
            shutdown_sidecar(sidecar_pid);
            if let Err(e) = update.download_and_install(|_chunk, _total| {}, || {}).await {
                eprintln!("[NGD_KF] 업데이트 설치 실패: {e}");
                app_handle
                    .dialog()
                    .message(format!("업데이트 설치 중 오류가 발생했습니다:\n{e}"))
                    .title("NGD_KF 업데이트 실패")
                    .buttons(MessageDialogButtons::Ok)
                    .blocking_show();
                return;
            }
            println!("[NGD_KF] 업데이트 설치 완료 - 재시작 시도(Windows에서는 대개 여기 도달하지 못하고 위 설치 단계에서 이미 종료됨).");
            app_handle.restart();
        }
        Ok(None) => println!("[NGD_KF] 이미 최신 버전입니다."),
        Err(e) => eprintln!("[NGD_KF] 업데이트 확인 실패(무시하고 계속 실행): {e}"),
    }
}

/// 실행 시점 1회 + UPDATE_CHECK_INTERVAL마다 반복 확인(별도 OS 스레드 -
/// sidecar 준비 폴링(wait_for_backend_and_navigate)과 같은 패턴으로,
/// tokio를 직접 의존성에 추가하지 않고 tauri::async_runtime::block_on만
/// 빌려 쓴다).
fn spawn_update_checker(app_handle: tauri::AppHandle) {
    std::thread::spawn(move || loop {
        tauri::async_runtime::block_on(check_for_update_and_prompt(&app_handle));
        std::thread::sleep(UPDATE_CHECK_INTERVAL);
    });
}

// ---- 파일 다운로드 처리(2026-10-06, 한글 출력 "무반응" 대응) ----
// 지금까지는 다운로드 처리기를 등록하지 않아서 WebView2 기본 동작에 맡겼다 - 그러면
// 아무 안내 없이 다운로드 폴더에 조용히 저장되거나(실제로 개발 PC 다운로드 폴더에
// ngd_export*.hwpx가 쌓여 있었음), 폴더가 없거나 쓸 수 없는 PC에서는 저장 자체가
// 실패해도 사용자는 모달이 그냥 닫힌 것밖에 못 본다. 그래서 (1)저장 위치를 우리가
// 정하고(다운로드 폴더, 없으면 만들고, 이름 충돌 시 " (n)" 붙임), (2)끝나면 저장
// 경로와 "폴더 열기"를 항상 화면에 알려준다(실패하면 실패했다고도 알려준다).

/// dir 안에서 name과 안 겹치는 경로("name (1).ext" ...)를 돌려준다.
fn unique_download_path(dir: &Path, name: &std::ffi::OsStr) -> PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let p = Path::new(name);
    let stem = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "download".into());
    let ext = p.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    for n in 1..1000 {
        let cand = dir.join(format!("{stem} ({n}){ext}"));
        if !cand.exists() {
            return cand;
        }
    }
    first
}

/// 저장할 폴더: 다운로드 폴더 -> 문서 폴더 -> 임시 폴더 순(앞이 없거나 만들 수 없으면 다음).
fn pick_download_dir(app: &tauri::AppHandle) -> PathBuf {
    let candidates = [
        app.path().download_dir().ok(),
        app.path().document_dir().ok(),
        Some(std::env::temp_dir()),
    ];
    for dir in candidates.into_iter().flatten() {
        if std::fs::create_dir_all(&dir).is_ok() {
            return dir;
        }
    }
    std::env::temp_dir()
}

/// 탐색기에서 파일을 선택한 상태로 연다(Windows 전용, 실패해도 무시).
fn reveal_in_explorer(path: &Path) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as WinCmdExt;
        let _ = StdCommand::new("explorer.exe")
            .raw_arg(format!("/select,\"{}\"", path.display()))
            .spawn();
    }
    #[cfg(not(windows))]
    {
        let _ = path;
    }
}

fn handle_download(app: &tauri::AppHandle, event: DownloadEvent<'_>) -> bool {
    match event {
        DownloadEvent::Requested { destination, .. } => {
            let name = destination
                .file_name()
                .map(|n| n.to_os_string())
                .unwrap_or_else(|| "ngd_export.hwpx".into());
            *destination = unique_download_path(&pick_download_dir(app), &name);
            println!("[NGD_KF] 다운로드 시작 -> {}", destination.display());
            true
        }
        DownloadEvent::Finished { path, success, .. } => {
            println!("[NGD_KF] 다운로드 종료: success={success} path={path:?}");
            let h = app.clone();
            // 다이얼로그는 다운로드 이벤트 콜백(웹뷰 스레드)을 막지 않도록 별도 스레드에서 띄운다.
            std::thread::spawn(move || match (success, path) {
                (true, Some(p)) => {
                    let open = h
                        .dialog()
                        .message(format!("파일이 저장되었습니다.\n\n{}", p.display()))
                        .title("저장 완료")
                        .buttons(MessageDialogButtons::OkCancelCustom(
                            "폴더 열기".into(),
                            "닫기".into(),
                        ))
                        .blocking_show();
                    if open {
                        reveal_in_explorer(&p);
                    }
                }
                _ => {
                    h.dialog()
                        .message("파일 저장에 실패했습니다.\n다운로드 폴더에 쓸 수 있는지 확인한 뒤 다시 시도해주세요.")
                        .title("저장 실패")
                        .buttons(MessageDialogButtons::Ok)
                        .blocking_show();
                }
            });
            true
        }
        _ => true,
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(SidecarState(Mutex::new(None)))
        .setup(|app| {
            // 메인 창을 코드에서 만든다(설정 파일엔 create:false로 두고 크기/제목/URL은 그대로
            // 설정에서 읽음) - 그래야 on_download로 다운로드 처리기를 붙일 수 있다.
            let main_cfg = app
                .config()
                .app
                .windows
                .iter()
                .find(|w| w.label == "main")
                .expect("tauri.conf.json에 label=main 창 설정이 없습니다")
                .clone();
            let dl_handle = app.handle().clone();
            WebviewWindowBuilder::from_config(app.handle(), &main_cfg)?
                .on_download(move |_webview, event| handle_download(&dl_handle, event))
                .build()?;

            let (mut _rx, child) = app
                .shell()
                .sidecar("NGD_KF")
                .expect("NGD_KF sidecar 바이너리를 못 찾았습니다(externalBin 설정 확인)")
                .spawn()
                .expect("NGD_KF sidecar 실행 실패");

            println!("[NGD_KF] sidecar 시작됨 (pid={})", child.pid());
            *app.state::<SidecarState>().0.lock().unwrap() = Some(child);

            wait_for_backend_and_navigate(app.handle().clone());
            spawn_update_checker(app.handle().clone());
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let app_handle = window.app_handle().clone();
                std::thread::spawn(move || {
                    let pid = app_handle
                        .state::<SidecarState>()
                        .0
                        .lock()
                        .unwrap()
                        .as_ref()
                        .map(|c| c.pid());
                    shutdown_sidecar(pid);
                    app_handle.exit(0);
                });
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
