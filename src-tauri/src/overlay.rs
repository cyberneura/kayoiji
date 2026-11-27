//! 候補ウインドウ。未確定文字列の近くに、フォーカスを奪わずに表示する。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use kayoiji_core::engine::{Mode, View};
use tauri::{
    AppHandle, Emitter, LogicalPosition, LogicalSize, Manager, State, WebviewUrl, WebviewWindow,
    WebviewWindowBuilder,
};

pub const LABEL: &str = "overlay";
const VIEW_EVENT: &str = "view";

/// 未確定文字列と候補ウインドウの間隔。
const GAP: f64 = 6.0;

/// 画面上の矩形。左上が原点の論理ピクセル。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

pub struct Overlay {
    /// 表示すべき状態かどうか。実際に表示するのは、内容の大きさが届いてから。
    wanted: AtomicBool,
    latest: Mutex<View>,
    /// 未確定文字列の先頭の位置。
    anchor: Mutex<Option<Rect>>,
}

impl Overlay {
    pub fn new(view: View) -> Self {
        Self {
            wanted: AtomicBool::new(false),
            latest: Mutex::new(view),
            anchor: Mutex::new(None),
        }
    }
}

pub fn create(app: &AppHandle) -> tauri::Result<WebviewWindow> {
    let window = WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App("overlay".into()))
        .title("Kayoiji")
        .inner_size(240.0, 40.0)
        .decorations(false)
        .resizable(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .visible_on_all_workspaces(true)
        .focused(false)
        .focusable(false)
        .visible(false)
        .build()?;
    // クリックを入力先のウインドウへ通す。候補は番号キーで選ぶ。
    window.set_ignore_cursor_events(true)?;
    Ok(window)
}

/// 表示内容を候補ウインドウへ反映する。`anchor` は未確定文字列の先頭の位置。
/// 候補ウインドウに出すものが無い表示内容かどうか。英文チェックでも、英文を受け取るまでは出さない。
pub fn is_hidden(view: &View) -> bool {
    view.mode == Mode::Direct || (view.mode == Mode::Checking && view.reading.is_empty())
}

pub fn update(app: &AppHandle, view: View, anchor: Option<Rect>) -> tauri::Result<()> {
    let overlay = app.state::<Overlay>();
    let Some(window) = app.get_webview_window(LABEL) else {
        return Ok(());
    };
    *overlay.latest.lock().expect("overlay lock") = view.clone();
    if is_hidden(&view) {
        overlay.wanted.store(false, Ordering::Relaxed);
        app.emit_to(LABEL, VIEW_EVENT, &view)?;
        return window.hide();
    }
    overlay.wanted.store(true, Ordering::Relaxed);
    if anchor.is_some() {
        *overlay.anchor.lock().expect("overlay lock") = anchor;
    }
    app.emit_to(LABEL, VIEW_EVENT, &view)
}

/// 候補ウインドウを隠す。`view` は確定した後の内容で、確定した文字列を最後の内容として残さないために置き換える。
pub fn hide(app: &AppHandle, view: View) -> tauri::Result<()> {
    let overlay = app.state::<Overlay>();
    *overlay.latest.lock().expect("overlay lock") = view.clone();
    app.emit_to(LABEL, VIEW_EVENT, &view)?;
    overlay.wanted.store(false, Ordering::Relaxed);
    match app.get_webview_window(LABEL) {
        Some(window) => window.hide(),
        None => Ok(()),
    }
}

/// 未確定文字列の下に置く。画面の下端からはみ出す時は上に置く。
fn position(anchor: Rect, width: f64, height: f64, screen: Option<Rect>) -> (f64, f64) {
    let below = anchor.y + anchor.height + GAP;
    let Some(screen) = screen else {
        return (anchor.x, below);
    };
    let y = if below + height > screen.y + screen.height {
        anchor.y - height - GAP
    } else {
        below
    };
    let x = anchor.x.min(screen.x + screen.width - width).max(screen.x);
    (x, y)
}

/// `anchor` がある画面の範囲を返す。
fn screen_containing(window: &WebviewWindow, anchor: Rect) -> Option<Rect> {
    window.available_monitors().ok()?.into_iter().find_map(|monitor| {
        let scale = monitor.scale_factor();
        let screen = Rect {
            x: monitor.position().x as f64 / scale,
            y: monitor.position().y as f64 / scale,
            width: monitor.size().width as f64 / scale,
            height: monitor.size().height as f64 / scale,
        };
        let contains = (screen.x..screen.x + screen.width).contains(&anchor.x)
            && (screen.y..screen.y + screen.height).contains(&anchor.y);
        contains.then_some(screen)
    })
}

#[tauri::command]
pub fn current_view(overlay: State<'_, Overlay>) -> View {
    overlay.latest.lock().expect("overlay lock").clone()
}

/// 候補ウインドウの内容の大きさ (論理ピクセル) を受け取り、ウインドウを合わせて表示する。
#[tauri::command]
pub fn overlay_resize(
    window: WebviewWindow,
    overlay: State<'_, Overlay>,
    width: f64,
    height: f64,
) -> Result<(), String> {
    if !overlay.wanted.load(Ordering::Relaxed) || width < 1.0 || height < 1.0 {
        return Ok(());
    }
    let apply = || -> tauri::Result<()> {
        window.set_size(LogicalSize::new(width, height))?;
        if let Some(anchor) = *overlay.anchor.lock().expect("overlay lock") {
            let (x, y) = position(anchor, width, height, screen_containing(&window, anchor));
            window.set_position(LogicalPosition::new(x, y))?;
        }
        window.show()
    };
    apply().map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCREEN: Rect = Rect {
        x: 0.0,
        y: 0.0,
        width: 1000.0,
        height: 800.0,
    };

    fn caret(x: f64, y: f64) -> Rect {
        Rect {
            x,
            y,
            width: 0.0,
            height: 20.0,
        }
    }

    #[test]
    fn window_goes_below_the_caret() {
        assert_eq!(position(caret(100.0, 200.0), 150.0, 250.0, Some(SCREEN)), (100.0, 226.0));
    }

    #[test]
    fn window_goes_above_the_caret_near_the_bottom_edge() {
        assert_eq!(position(caret(100.0, 700.0), 150.0, 250.0, Some(SCREEN)), (100.0, 444.0));
    }

    #[test]
    fn window_stays_inside_the_right_edge() {
        assert_eq!(position(caret(950.0, 200.0), 150.0, 250.0, Some(SCREEN)), (850.0, 226.0));
    }

    #[test]
    fn unknown_screen_keeps_the_caret_position() {
        assert_eq!(position(caret(950.0, 700.0), 150.0, 250.0, None), (950.0, 726.0));
    }
}
