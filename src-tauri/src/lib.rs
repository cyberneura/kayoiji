#[cfg(target_os = "macos")]
mod imk;
#[cfg(target_os = "macos")]
mod keys;
mod overlay;
mod settings;

use std::error::Error;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use kayoiji_core::engine::{Engine, EngineConfig};
use kayoiji_core::learning::Learning;
use kayoiji_core::resources::{load_rules, open_engine};
use serde::Serialize;
use tauri::path::BaseDirectory;
use tauri::{App, AppHandle, Emitter, Manager, RunEvent, State, WebviewUrl, WebviewWindowBuilder};

use overlay::Overlay;
use settings::Settings;

const SETTINGS_WINDOW: &str = "settings";
const ABOUT_WINDOW: &str = "about";
/// 設定が変わったことを、開いている設定画面に知らせるイベント。
const SETTINGS_EVENT: &str = "settings-changed";

/// 辞書があるディレクトリを差し替える環境変数。開発時に使う。
const DATA_DIR_VARIABLE: &str = "KAYOIJI_DATA_DIR";

struct AppState {
    engine: Arc<Mutex<Engine>>,
    settings: Mutex<Settings>,
    settings_path: PathBuf,
    rules_path: PathBuf,
    /// OS の入力メソッドとして動いているかどうか。
    input_method_running: bool,
    /// 起動時に読めなかった利用者のファイルと、その理由。
    startup_warnings: Vec<String>,
}

#[derive(Serialize)]
struct AppStatus {
    settings: Settings,
    input_method_running: bool,
    version: String,
    /// ビルドした時刻 (UNIX 時間の秒)。
    built_at: u64,
    rules_path: String,
    warnings: Vec<String>,
}

/// ビルドした時刻 (UNIX 時間の秒)。`build.rs` が埋め込む。
fn built_at() -> u64 {
    env!("KAYOIJI_BUILT_AT")
        .parse()
        .expect("build.rs writes the time as a number")
}

#[tauri::command]
fn app_status(app: AppHandle, state: State<'_, AppState>) -> AppStatus {
    AppStatus {
        settings: state.settings.lock().expect("settings lock").clone(),
        input_method_running: state.input_method_running,
        version: app.package_info().version.to_string(),
        built_at: built_at(),
        rules_path: state.rules_path.display().to_string(),
        warnings: state.startup_warnings.clone(),
    }
}

#[tauri::command]
fn update_settings(app: AppHandle, settings: Settings) -> Result<(), String> {
    store_settings(&app, settings)
}

/// 設定を保存して、エンジンに反映する。
fn store_settings(app: &AppHandle, settings: Settings) -> Result<(), String> {
    let state = app.state::<AppState>();
    settings.validate()?;
    settings.save(&state.settings_path).map_err(|error| error.to_string())?;
    let config = settings.engine_config();
    *state.settings.lock().expect("settings lock") = settings;
    apply_engine_config(app, Arc::clone(&state.engine), config);
    // 入力メニューから変えた設定を、開いている設定画面に反映する。設定は保存できているので、
    // 知らせるのに失敗しても、保存の失敗にはしない。
    if let Err(error) = app.emit_to(SETTINGS_WINDOW, SETTINGS_EVENT, ()) {
        eprintln!("kayoiji: could not notify the settings window: {error}");
    }
    Ok(())
}

/// 入力モードが変わると、入力中の文字列を入力先に確定する。入力先を呼ぶのはメインスレッドに限る。
#[cfg(target_os = "macos")]
fn apply_engine_config(app: &AppHandle, engine: Arc<Mutex<Engine>>, config: EngineConfig) {
    if let Err(error) = app.run_on_main_thread(move || imk::set_config(&engine, config)) {
        eprintln!("kayoiji: could not apply the settings: {error}");
    }
}

#[cfg(not(target_os = "macos"))]
fn apply_engine_config(_app: &AppHandle, engine: Arc<Mutex<Engine>>, config: EngineConfig) {
    engine.lock().expect("engine lock").set_config(config);
}

pub(crate) fn auto_switch_enabled(app: &AppHandle) -> bool {
    app.state::<AppState>().settings.lock().expect("settings lock").auto_switch
}

/// 自動切り替えを、有効なら無効に、無効なら有効にする。
pub(crate) fn toggle_auto_switch(app: &AppHandle) -> Result<(), String> {
    let mut settings = app.state::<AppState>().settings.lock().expect("settings lock").clone();
    settings.auto_switch = !settings.auto_switch;
    store_settings(app, settings)
}

fn data_dir(app: &App) -> tauri::Result<PathBuf> {
    match std::env::var_os(DATA_DIR_VARIABLE) {
        Some(directory) => Ok(PathBuf::from(directory)),
        None => app.path().resolve("data", BaseDirectory::Resource),
    }
}

pub(crate) fn show_settings_window(app: &AppHandle) -> tauri::Result<()> {
    let window = match app.get_webview_window(SETTINGS_WINDOW) {
        Some(window) => window,
        None => WebviewWindowBuilder::new(app, SETTINGS_WINDOW, WebviewUrl::App("settings".into()))
            .title("Kayoiji")
            .inner_size(600.0, 620.0)
            .build()?,
    };
    window.show()?;
    window.set_focus()
}

pub(crate) fn show_about_window(app: &AppHandle) -> tauri::Result<()> {
    let window = match app.get_webview_window(ABOUT_WINDOW) {
        Some(window) => window,
        None => WebviewWindowBuilder::new(app, ABOUT_WINDOW, WebviewUrl::App("about".into()))
            .title("About Kayoiji")
            .inner_size(340.0, 200.0)
            .resizable(false)
            .minimizable(false)
            .maximizable(false)
            .build()?,
    };
    window.show()?;
    window.set_focus()
}

fn setup(app: &mut App) -> Result<(), Box<dyn Error>> {
    // 入力メソッドは Dock にもアプリ切り替えにも出さない。
    #[cfg(target_os = "macos")]
    app.set_activation_policy(tauri::ActivationPolicy::Accessory);

    let config_dir = app.path().app_config_dir()?;
    let settings_path = config_dir.join("settings.toml");
    let rules_path = config_dir.join("rules.toml");
    let learning_path = app.path().app_data_dir()?.join("learning.json");

    // 利用者のファイルが読めなくても、入力メソッドは起動させる。起動しないと、選んでも何も打てなくなる。
    // 読めなかったファイルは既定の内容で代用して、設定画面に理由を出す。
    let mut startup_warnings = Vec::new();
    let settings = Settings::load(&settings_path).unwrap_or_else(|error| {
        startup_warnings.push(format!("{}: {error}", settings_path.display()));
        Settings::default()
    });
    let (rules, rules_error) = load_rules(&rules_path);
    startup_warnings.extend(rules_error.map(|error| error.to_string()));
    let (learning, learning_error) = Learning::load_or_set_aside(&learning_path);
    startup_warnings.extend(learning_error.map(|error| format!("{}: {error}", learning_path.display())));
    for warning in &startup_warnings {
        eprintln!("kayoiji: {warning}");
    }

    let engine = open_engine(&data_dir(app)?, rules, learning, settings.engine_config())?;
    let view = engine.view();
    let engine = Arc::new(Mutex::new(engine));

    app.manage(Overlay::new(view));
    overlay::create(app.handle())?;

    #[cfg(target_os = "macos")]
    let input_method_running = imk::start(app.handle().clone(), Arc::clone(&engine));
    #[cfg(not(target_os = "macos"))]
    let input_method_running = false;

    app.manage(AppState {
        engine,
        settings: Mutex::new(settings),
        settings_path,
        rules_path,
        input_method_running,
        startup_warnings,
    });
    // 入力メソッドとして起動されていない時 (開発時など) は、設定画面を開く。
    if !input_method_running {
        show_settings_window(app.handle())?;
    }
    Ok(())
}

pub fn run() {
    tauri::Builder::default()
        .setup(setup)
        .invoke_handler(tauri::generate_handler![
            app_status,
            update_settings,
            overlay::current_view,
            overlay::overlay_resize
        ])
        .build(tauri::generate_context!())
        .expect("error while building the Kayoiji application")
        .run(|_app, event| {
            // 設定ウインドウを閉じても、入力メソッドとして動き続ける。
            if let RunEvent::ExitRequested { api, code: None, .. } = event {
                api.prevent_exit();
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_build_time_is_embedded_as_seconds() {
        // 2026-01-01 00:00:00 UTC。ミリ秒や 0 が埋め込まれていれば、ここで分かる。
        let earliest = 1_767_225_600;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("the clock is after 1970")
            .as_secs();

        let built_at = built_at();

        assert!((earliest..=now).contains(&built_at), "built_at = {built_at}");
    }
}
