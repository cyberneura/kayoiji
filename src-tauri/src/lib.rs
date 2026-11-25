#[cfg(target_os = "macos")]
mod imk;
#[cfg(target_os = "macos")]
mod keys;
mod overlay;
mod settings;

use std::error::Error;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use kayoiji_core::engine::Engine;
use kayoiji_core::learning::Learning;
use kayoiji_core::resources::{load_rules, open_engine};
use serde::Serialize;
use tauri::path::BaseDirectory;
use tauri::{App, AppHandle, Manager, RunEvent, State, WebviewUrl, WebviewWindowBuilder};

use overlay::Overlay;
use settings::Settings;

const SETTINGS_WINDOW: &str = "settings";

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
    rules_path: String,
    warnings: Vec<String>,
}

#[tauri::command]
fn app_status(app: AppHandle, state: State<'_, AppState>) -> AppStatus {
    AppStatus {
        settings: state.settings.lock().expect("settings lock").clone(),
        input_method_running: state.input_method_running,
        version: app.package_info().version.to_string(),
        rules_path: state.rules_path.display().to_string(),
        warnings: state.startup_warnings.clone(),
    }
}

#[tauri::command]
fn update_settings(state: State<'_, AppState>, settings: Settings) -> Result<(), String> {
    settings.validate()?;
    settings.save(&state.settings_path).map_err(|error| error.to_string())?;
    state
        .engine
        .lock()
        .expect("engine lock")
        .set_config(settings.engine_config());
    *state.settings.lock().expect("settings lock") = settings;
    Ok(())
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
            .inner_size(600.0, 560.0)
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
