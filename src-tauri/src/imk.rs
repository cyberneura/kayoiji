//! macOS の InputMethodKit との接続。
//!
//! このプロセスの中で `IMKServer` を動かす。入力先のアプリケーションごとに、OS が
//! `KayoijiInputController` のインスタンスを作って打鍵を渡してくる。

use std::cell::RefCell;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use kayoiji_core::engine::{Engine, EngineConfig, InputMode, Key, Mode, Output, View};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Sel};
use objc2::{define_class, msg_send, sel, AllocAnyThread, ClassType, MainThreadMarker};
use objc2_app_kit::{
    NSControlStateValueOn, NSEvent, NSEventMask, NSEventModifierFlags, NSEventType, NSMenu, NSMenuItem, NSScreen,
};
use objc2_foundation::{ns_string, NSAttributedString, NSBundle, NSNotFound, NSRange, NSRect, NSString};
use objc2_input_method_kit::{IMKInputController, IMKServer};
use tauri::AppHandle;

use crate::keys::{translate, KeyPress};
use crate::overlay::{self, Rect};

/// Info.plist で、入力メソッドの接続名を指定するキー。
const CONNECTION_NAME_KEY: &str = "InputMethodConnectionName";
/// Info.plist の `ComponentInputModeDict` に書いた、入力モードの ID。メニューバーのアイコンはモードごとに決まる。
const JAPANESE_MODE_ID: &str = "com.cyberneura.inputmethod.Kayoiji.Japanese";
const AUTO_MODE_ID: &str = "com.cyberneura.inputmethod.Kayoiji.Auto";
const DIRECT_MODE_ID: &str = "com.cyberneura.inputmethod.Kayoiji.Roman";
/// `setValue:forTag:client:` で、入力モードの切り替えを表すタグ (`kTextServiceInputModePropertyTag`、'imod')。
const INPUT_MODE_TAG: isize = 0x696D_6F64;
/// OS が入力モードを切り替えた直後に届く「かな」キーを、切り替えの一部とみなす時間。
/// 自動切り替えでこのキーを打鍵として扱うと、切り替えた直後に「次の入力を日本語にする」指定が入ってしまう。
/// ただし、OS が選んだのが設定と違う側のモードなら、このキーで設定の側のモードに入り直す。
const MODE_SWITCH_SETTLE: Duration = Duration::from_millis(300);

struct Runtime {
    app: AppHandle,
    engine: Arc<Mutex<Engine>>,
    /// 打鍵の時刻の基準。
    started: Instant,
    /// 打鍵を受け取っている入力先。打鍵の間隔が空いた時の確定に使う。
    client: Option<Retained<AnyObject>>,
    /// 入力先に未確定文字列を表示しているかどうか。
    has_preedit: bool,
    last_view: View,
    /// OS が最後に入力モードを切り替えた時刻。
    mode_switched_at: Option<Instant>,
    /// 入力先の切り替えなどで、未確定文字列をまとめて確定した回数。
    commit_count: u64,
    /// 保持していないと、OS からの接続を受けられなくなる。
    _server: Retained<IMKServer>,
}

thread_local! {
    // InputMethodKit のコールバックは、すべてメインスレッドで呼ばれる。
    static RUNTIME: RefCell<Option<Runtime>> = const { RefCell::new(None) };
}

define_class!(
    // SAFETY: `IMKInputController` はサブクラス化を前提としたクラス。インスタンス変数は持たない。
    #[unsafe(super(IMKInputController))]
    #[name = "KayoijiInputController"]
    struct InputController;

    impl InputController {
        #[unsafe(method(recognizedEvents:))]
        fn recognized_events(&self, _client: Option<&AnyObject>) -> usize {
            NSEventMask::KeyDown.0 as usize
        }

        #[unsafe(method(handleEvent:client:))]
        fn handle_event(&self, event: Option<&NSEvent>, client: Option<&AnyObject>) -> bool {
            match (event, client) {
                (Some(event), Some(client)) => handle_key(event, client),
                _ => false,
            }
        }

        /// 入力先が切り替わる時や、未確定文字列の外をクリックした時に呼ばれる。
        #[unsafe(method(commitComposition:))]
        fn commit_composition(&self, client: Option<&AnyObject>) {
            commit_all(client);
        }

        #[unsafe(method(deactivateServer:))]
        fn deactivate_server(&self, client: Option<&AnyObject>) {
            commit_all(client);
        }

        /// 入力メニューや「英数」「かな」キーで、OS が入力モードを切り替えた時に呼ばれる。
        #[unsafe(method(setValue:forTag:client:))]
        fn set_value(&self, value: Option<&AnyObject>, tag: isize, client: Option<&AnyObject>) {
            if tag == INPUT_MODE_TAG {
                mode_selected(value, client);
            }
        }

        #[unsafe(method_id(menu))]
        fn menu(&self) -> Option<Retained<NSMenu>> {
            input_menu()
        }

        #[unsafe(method(showPreferences:))]
        fn show_preferences(&self, _sender: Option<&AnyObject>) {
            show_window(crate::show_settings_window);
        }

        #[unsafe(method(showAbout:))]
        fn show_about(&self, _sender: Option<&AnyObject>) {
            show_window(crate::show_about_window);
        }

        #[unsafe(method(toggleAutoSwitch:))]
        fn toggle_auto_switch(&self, _sender: Option<&AnyObject>) {
            let Some(app) = app_handle() else {
                return;
            };
            // 入力メソッドを選んでから 1 度も打鍵していないと、入力先をまだ覚えていない。
            // 覚えていないと、切り替えた入力モードを OS に伝えられず、アイコンが変わらない。
            // SAFETY: `client` は IMKInputController のメソッドで、今の入力先か nil を返す。
            let client: Option<Retained<AnyObject>> = unsafe { msg_send![self, client] };
            RUNTIME.with(|runtime| {
                if let Some(runtime) = runtime.borrow_mut().as_mut() {
                    runtime.client = runtime.client.take().or(client);
                }
            });
            if let Err(error) = crate::toggle_auto_switch(&app) {
                eprintln!("kayoiji: could not save the settings: {error}");
            }
        }
    }
);

fn app_handle() -> Option<AppHandle> {
    RUNTIME.with(|runtime| runtime.borrow().as_ref().map(|runtime| runtime.app.clone()))
}

/// 入力メニュー (メニューバーの入力ソースのメニュー) に足す項目。
/// InputMethodKit は、項目のアクションをコントローラーへ届ける。
fn input_menu() -> Option<Retained<NSMenu>> {
    let mtm = MainThreadMarker::new()?;
    let app = app_handle()?;
    let menu = NSMenu::new(mtm);
    let auto_switch = menu_item(mtm, ns_string!("英語と日本語を自動で切り替える"), sel!(toggleAutoSwitch:));
    if crate::auto_switch_enabled(&app) {
        auto_switch.setState(NSControlStateValueOn);
    }
    menu.addItem(&auto_switch);
    menu.addItem(&NSMenuItem::separatorItem(mtm));
    menu.addItem(&menu_item(mtm, ns_string!("Kayoiji の設定…"), sel!(showPreferences:)));
    menu.addItem(&menu_item(mtm, ns_string!("About Kayoiji"), sel!(showAbout:)));
    Some(menu)
}

fn menu_item(mtm: MainThreadMarker, title: &NSString, action: Sel) -> Retained<NSMenuItem> {
    // SAFETY: `action` は、どれも `InputController` に定義したメソッドのセレクター。
    unsafe { NSMenuItem::initWithTitle_action_keyEquivalent(mtm.alloc(), title, Some(action), ns_string!("")) }
}

fn show_window(show: fn(&AppHandle) -> tauri::Result<()>) {
    let Some(app) = app_handle() else {
        return;
    };
    if let Err(error) = show(&app) {
        eprintln!("kayoiji: window: {error}");
    }
}

/// 設定をエンジンに反映する。入力モードが変わる時は、入力中の文字列を入力先に確定する。
/// メインスレッドで呼ぶ。
pub fn set_config(engine: &Mutex<Engine>, config: EngineConfig) {
    let (output, view, new_mode) = {
        let mut engine = engine.lock().expect("engine lock");
        let mode_before = engine.input_mode();
        let output = engine.set_config(config);
        let mode = engine.input_mode();
        (output, engine.view(), (mode != mode_before).then_some(mode))
    };
    let client = RUNTIME.with(|runtime| runtime.borrow().as_ref().and_then(|runtime| runtime.client.clone()));
    let Some(client) = client else {
        return;
    };
    apply(&client, output);
    refresh_overlay(&client, view);
    if let Some(mode) = new_mode {
        select_mode(&client, mode);
    }
}

const MODES: [InputMode; 3] = [InputMode::Japanese, InputMode::Auto, InputMode::Direct];

fn mode_id(mode: InputMode) -> &'static str {
    match mode {
        InputMode::Japanese => JAPANESE_MODE_ID,
        InputMode::Auto => AUTO_MODE_ID,
        InputMode::Direct => DIRECT_MODE_ID,
    }
}

/// エンジンの入力モードを OS に伝える。メニューバーのアイコンが変わる。
fn select_mode(client: &AnyObject, mode: InputMode) {
    let identifier = NSString::from_str(mode_id(mode));
    // SAFETY: `selectInputMode:` は IMKTextInput のメソッド。
    let _: () = unsafe { msg_send![client, selectInputMode: &*identifier] };
}

/// OS が選んだ入力モードに対応する、エンジンのモード。
fn mode_of(identifier: &str) -> Option<InputMode> {
    MODES.into_iter().find(|&mode| mode_id(mode) == identifier)
}

/// 入力メソッドのサーバーを起動する。入力メソッドのバンドルとして起動されていなければ `false` を返す。
pub fn start(app: AppHandle, engine: Arc<Mutex<Engine>>) -> bool {
    let bundle = NSBundle::mainBundle();
    let connection = bundle
        .objectForInfoDictionaryKey(&NSString::from_str(CONNECTION_NAME_KEY))
        .and_then(|value| value.downcast::<NSString>().ok());
    let (Some(connection), Some(identifier)) = (connection, bundle.bundleIdentifier()) else {
        return false;
    };
    // Info.plist のクラス名から探せるように、サーバーを作る前にクラスを登録する。
    let _ = InputController::class();
    // SAFETY: 接続名とバンドル ID は、どちらも実在する文字列。
    let server = unsafe {
        IMKServer::initWithName_bundleIdentifier(IMKServer::alloc(), Some(&connection), Some(&identifier))
    };
    let Some(server) = server else {
        return false;
    };
    let last_view = engine.lock().expect("engine lock").view();
    RUNTIME.with(|runtime| {
        *runtime.borrow_mut() = Some(Runtime {
            app,
            engine,
            started: Instant::now(),
            client: None,
            has_preedit: false,
            last_view,
            mode_switched_at: None,
            commit_count: 0,
            _server: server,
        });
    });
    true
}

fn key_press(event: &NSEvent) -> KeyPress {
    let flags = event.modifierFlags();
    let character = event
        .charactersIgnoringModifiers()
        .and_then(|characters| characters.to_string().chars().next());
    KeyPress {
        key_code: event.keyCode(),
        character,
        shift: flags.contains(NSEventModifierFlags::Shift),
        control: flags.contains(NSEventModifierFlags::Control),
        option: flags.contains(NSEventModifierFlags::Option),
        command: flags.contains(NSEventModifierFlags::Command),
        repeat: event.isARepeat(),
    }
}

fn handle_key(event: &NSEvent, client: &AnyObject) -> bool {
    if event.r#type() != NSEventType::KeyDown {
        return false;
    }
    let press = key_press(event);
    // 入力先を呼び出している間は RUNTIME を借用しない。入力先からの呼び出しが
    // このコントローラーへ戻ってくることがある。
    let prepared = RUNTIME.with(|runtime| {
        let runtime = runtime.borrow();
        let engine = runtime.as_ref()?.engine.lock().expect("engine lock");
        let key = translate(press, engine.view().mode);
        let reread = if key == Key::ForceJapanese {
            engine.reread_target()
        } else {
            String::new()
        };
        Some((key, reread))
    });
    let Some((key, reread)) = prepared else {
        return false;
    };
    // エンジンには渡さないが、入力先にも通さない。通すと、このキーで空白を入力する入力先がある。
    if key == Key::Henkan && settles_mode_switch() {
        return true;
    }
    // 確定済みの単語の読み直しは、その単語がキャレットの直前にある時だけ行う。無ければエンジンに
    // 渡さずに、キーを入力先へ通す。渡してしまうと、単語を日本語として覚えた上に、未確定文字列が
    // その時の選択範囲を置き換える。
    if !reread.is_empty() && reread_range(client, &reread).is_none() {
        return false;
    }
    let handled = RUNTIME.with(|runtime| {
        let mut runtime = runtime.borrow_mut();
        let runtime = runtime.as_mut()?;
        // SAFETY: `client` は、この呼び出しの間有効な入力先のオブジェクト。
        runtime.client = unsafe { Retained::retain(client as *const AnyObject as *mut AnyObject) };
        let now = runtime.started.elapsed().as_millis() as u64;
        let mut engine = runtime.engine.lock().expect("engine lock");
        let mode_before = engine.input_mode();
        let output = engine.handle(key, now);
        if let Some(error) = engine.take_error() {
            eprintln!("kayoiji: could not save what was learned: {error}");
        }
        let deadline = engine.timeout_deadline().map(|deadline| deadline.saturating_sub(now));
        let mode = engine.input_mode();
        Some((output, engine.view(), deadline, (mode != mode_before).then_some(mode)))
    });
    let Some((output, view, deadline, new_mode)) = handled else {
        return false;
    };
    let consumed = output.consumed;
    apply(client, output);
    refresh_overlay(client, view);
    if let Some(mode) = new_mode {
        select_mode(client, mode);
    }
    if let Some(delay) = deadline {
        schedule_timeout(Duration::from_millis(delay + 1));
    }
    consumed
}

/// 今届いた「かな」キーが、OS が済ませた入力モードの切り替えの一部かどうか。
///
/// OS は、日本語入力と自動切り替えのどちらが設定されているかを知らない。「かな」キーで OS が選んだのが
/// 設定と違う側のモードなら、切り替えはまだ済んでいない。エンジンに渡して、設定の側のモードに入り直す。
fn settles_mode_switch() -> bool {
    RUNTIME.with(|runtime| {
        let runtime = runtime.borrow();
        let Some(runtime) = runtime.as_ref() else {
            return false;
        };
        let just_switched = runtime
            .mode_switched_at
            .is_some_and(|switched| switched.elapsed() < MODE_SWITCH_SETTLE);
        let engine = runtime.engine.lock().expect("engine lock");
        just_switched && engine.input_mode() == engine.kana_key_mode()
    })
}

/// 読み直す単語がキャレットの直前にあれば、その範囲を返す。選択範囲がある時は `None`。
///
/// 単語を確定した後に利用者がクリックでキャレットを動かしても、入力メソッドには通知されない。
/// 文字数だけで消すと別の文字列を消してしまうので、直前の文字列が一致する時だけ置き換える。
/// 直前の文字列を返さない入力先では、確定済みの単語の読み直しはできない。
fn reread_range(client: &AnyObject, reread: &str) -> Option<NSRange> {
    let length = reread.encode_utf16().count();
    // SAFETY: `selectedRange` は IMKTextInput のメソッドで、NSRange を返す。
    let selected: NSRange = unsafe { msg_send![client, selectedRange] };
    if selected.location == NSNotFound as usize || selected.length != 0 || selected.location < length {
        return None;
    }
    let range = NSRange::new(selected.location - length, length);
    // SAFETY: `attributedSubstringFromRange:` は IMKTextInput のメソッドで、NSAttributedString か nil を返す。
    let before: Option<Retained<NSAttributedString>> =
        unsafe { msg_send![client, attributedSubstringFromRange: range] };
    (before?.string().to_string() == reread).then_some(range)
}

/// エンジンの出力を入力先へ反映する。
fn apply(client: &AnyObject, output: Output) {
    let state = RUNTIME.with(|runtime| {
        let mut runtime = runtime.borrow_mut();
        let runtime = runtime.as_mut()?;
        let had_preedit = std::mem::replace(&mut runtime.has_preedit, !output.preedit.is_empty());
        Some((had_preedit, runtime.commit_count))
    });
    let Some((had_preedit, commit_count)) = state else {
        return;
    };
    let not_found = NSRange::new(NSNotFound as usize, 0);
    let mut replacement = not_found;
    if !output.reread.is_empty() {
        replacement = reread_range(client, &output.reread).unwrap_or(not_found);
    }
    if !output.commit.is_empty() {
        let text = NSString::from_str(&output.commit);
        // SAFETY: `insertText:replacementRange:` は IMKTextInput のメソッド。文字列は呼び出しの間有効。
        let _: () = unsafe { msg_send![client, insertText: &*text, replacementRange: replacement] };
        replacement = not_found;
        // 入力先が確定の最中に `commitComposition:` を呼び返すと、その中でエンジンの未確定文字列は
        // 確定されて無くなる。ここで古い未確定文字列を表示すると、エンジンと食い違ったまま残る。
        let committed_inside = RUNTIME.with(|runtime| {
            runtime
                .borrow()
                .as_ref()
                .is_none_or(|runtime| runtime.commit_count != commit_count)
        });
        if committed_inside {
            return;
        }
    }
    // 確定すると未確定文字列は消える。何も確定せずに空になった時は、明示的に消す。
    let must_clear = had_preedit && output.commit.is_empty();
    if output.preedit.is_empty() && !must_clear && replacement.location == NSNotFound as usize {
        return;
    }
    let text = NSString::from_str(&output.preedit);
    // 変換中は、候補を選んでいる文節を選択範囲として示す。それ以外は末尾にキャレットを置く。
    let selection = match output.focus {
        Some((start, length)) => {
            let utf16_len = |skip: usize, take: usize| -> usize {
                output.preedit.chars().skip(skip).take(take).map(char::len_utf16).sum()
            };
            NSRange::new(utf16_len(0, start), utf16_len(start, length))
        }
        None => NSRange::new(text.length(), 0),
    };
    // SAFETY: `setMarkedText:selectionRange:replacementRange:` は IMKTextInput のメソッド。
    // 文字列と範囲は呼び出しの間有効。
    let _: () = unsafe {
        msg_send![
            client,
            setMarkedText: &*text,
            selectionRange: selection,
            replacementRange: replacement
        ]
    };
}

/// 未確定文字列の先頭の位置を、左上を原点とする座標で返す。
fn preedit_rect(client: &AnyObject) -> Option<Rect> {
    let mtm = MainThreadMarker::new()?;
    let mut rect = NSRect::ZERO;
    // SAFETY: `attributesForCharacterIndex:lineHeightRectangle:` は IMKTextInput のメソッド。
    // `rect` は呼び出しの間有効で、戻り値の辞書は使わない。
    let _: Option<Retained<AnyObject>> =
        unsafe { msg_send![client, attributesForCharacterIndex: 0usize, lineHeightRectangle: &mut rect] };
    if rect.size.height <= 0.0 {
        return None;
    }
    // AppKit の座標は、主画面の左下が原点で上向き。
    let primary_height = NSScreen::screens(mtm).firstObject()?.frame().size.height;
    Some(Rect {
        x: rect.origin.x,
        y: primary_height - rect.origin.y - rect.size.height,
        width: rect.size.width,
        height: rect.size.height,
    })
}

fn refresh_overlay(client: &AnyObject, view: View) {
    let app = RUNTIME.with(|runtime| {
        let mut runtime = runtime.borrow_mut();
        let runtime = runtime.as_mut()?;
        if runtime.last_view == view {
            return None;
        }
        runtime.last_view = view.clone();
        Some(runtime.app.clone())
    });
    let Some(app) = app else {
        return;
    };
    let anchor = if view.mode == Mode::Direct {
        None
    } else {
        preedit_rect(client)
    };
    if let Err(error) = overlay::update(&app, view, anchor) {
        eprintln!("kayoiji: candidate window: {error}");
    }
}

/// 表示中の未確定文字列を、そのまま確定する。
fn commit_all(client: Option<&AnyObject>) {
    let committed = RUNTIME.with(|runtime| {
        let mut runtime = runtime.borrow_mut();
        let runtime = runtime.as_mut()?;
        runtime.client = None;
        runtime.commit_count += 1;
        let mut engine = runtime.engine.lock().expect("engine lock");
        let commit = engine.commit_composition();
        Some((commit, engine.view(), runtime.app.clone()))
    });
    let Some((commit, view, app)) = committed else {
        return;
    };
    if let Some(client) = client {
        apply(
            client,
            Output {
                consumed: true,
                reread: String::new(),
                commit,
                preedit: String::new(),
                focus: None,
            },
        );
    }
    RUNTIME.with(|runtime| {
        if let Some(runtime) = runtime.borrow_mut().as_mut() {
            runtime.last_view = view;
        }
    });
    if let Err(error) = overlay::hide(&app) {
        eprintln!("kayoiji: candidate window: {error}");
    }
}

/// OS が切り替えた入力モードを、エンジンに反映する。
fn mode_selected(value: Option<&AnyObject>, client: Option<&AnyObject>) {
    let Some(mode) = value
        .and_then(|value| value.downcast_ref::<NSString>())
        .and_then(|identifier| mode_of(&identifier.to_string()))
    else {
        return;
    };
    let switched = RUNTIME.with(|runtime| {
        let mut runtime = runtime.borrow_mut();
        let runtime = runtime.as_mut()?;
        let mut engine = runtime.engine.lock().expect("engine lock");
        // 同じモードの通知 (キーで切り替えた時に OS へ伝えた分の折り返しなど) は、切り替えとして扱わない。
        if engine.input_mode() == mode {
            return None;
        }
        runtime.mode_switched_at = Some(Instant::now());
        Some((engine.set_input_mode(mode), engine.view()))
    });
    let (Some((output, view)), Some(client)) = (switched, client) else {
        return;
    };
    apply(client, output);
    refresh_overlay(client, view);
}

/// 打鍵の間隔が空いたら、判定中の単語を英語として確定する。
fn schedule_timeout(delay: Duration) {
    let app = RUNTIME.with(|runtime| runtime.borrow().as_ref().map(|runtime| runtime.app.clone()));
    let Some(app) = app else {
        return;
    };
    thread::spawn(move || {
        thread::sleep(delay);
        let _ = app.run_on_main_thread(fire_timeout);
    });
}

fn fire_timeout() {
    let fired = RUNTIME.with(|runtime| {
        let mut runtime = runtime.borrow_mut();
        let runtime = runtime.as_mut()?;
        let client = runtime.client.clone()?;
        let now = runtime.started.elapsed().as_millis() as u64;
        let mut engine = runtime.engine.lock().expect("engine lock");
        // 予約した後に打鍵があれば、期限は先へ延びている。その打鍵が予約した分に任せる。
        if engine.timeout_deadline().is_none_or(|deadline| now <= deadline) {
            return None;
        }
        Some((client, engine.handle_timeout(now)))
    });
    if let Some((client, output)) = fired {
        apply(&client, output);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_input_mode_is_declared_in_the_info_plist() {
        // Arrange
        let info_plist = include_str!("../Info.plist");

        for mode in MODES {
            // Act
            let identifier = mode_id(mode);

            // Assert
            // ID が食い違うと、OS は切り替えを黙って無視する。メニューバーのアイコンが変わらなくなる。
            let declared = info_plist
                .split_once(&format!("<key>{identifier}</key>"))
                .and_then(|(_, mode)| mode.split_once("<key>TISInputSourceID</key>"))
                .and_then(|(_, rest)| rest.split_once("<string>"))
                .and_then(|(_, rest)| rest.split_once("</string>"))
                .map(|(source_id, _)| source_id);
            assert_eq!(declared, Some(identifier));
            let order = info_plist
                .split_once("<key>tsVisibleInputModeOrderedArrayKey</key>")
                .map(|(_, order)| order);
            assert!(order.is_some_and(|order| order.contains(&format!("<string>{identifier}</string>"))));
        }
    }

    #[test]
    fn an_input_mode_chosen_by_the_os_maps_back_to_the_engine_mode() {
        for mode in MODES {
            assert_eq!(mode_of(mode_id(mode)), Some(mode));
        }
        assert_eq!(mode_of("com.apple.keylayout.ABC"), None);
    }
}
