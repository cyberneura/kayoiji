//! macOS の InputMethodKit との接続。
//!
//! このプロセスの中で `IMKServer` を動かす。入力先のアプリケーションごとに、OS が
//! `KayoijiInputController` のインスタンスを作って打鍵を渡してくる。

use std::cell::RefCell;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use kayoiji_check::review::{Event, Review, Reviewer};
use kayoiji_check::spelling::{self, Guesses, Spelling};
use kayoiji_core::engine::{Engine, EngineConfig, InputMode, Key, Mode, Output, View};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Sel};
use objc2::{define_class, msg_send, sel, AllocAnyThread, ClassType, MainThreadMarker};
use objc2_app_kit::{
    NSControlStateValueOn, NSEvent, NSEventMask, NSEventModifierFlags, NSEventType, NSMenu, NSMenuItem, NSScreen,
};
use objc2_foundation::{ns_string, NSAttributedString, NSBundle, NSNotFound, NSRange, NSRect, NSString};
use objc2_input_method_kit::{IMKInputController, IMKServer};
use serde::Serialize;
use tauri::{AppHandle, Emitter};

use crate::input_source;
use crate::keys::{translate, KeyPress};
use crate::overlay::{self, Rect};

/// Info.plist で、入力メソッドの接続名を指定するキー。
const CONNECTION_NAME_KEY: &str = "InputMethodConnectionName";
/// Info.plist の `ComponentInputModeDict` に書いた、入力モードの ID。メニューバーのアイコンは入力モードごとに決まる。
/// 入力ソースの一覧に Kayoiji が 1 つだけ並ぶように、有効にするのは設定に合う側の 1 つだけにする
/// (`sync_input_mode`)。直接入力に OS の入力モードは無く、OS の英字のキーボード配列に任せる。
const JAPANESE_MODE_ID: &str = "com.cyberneura.inputmethod.Kayoiji.Japanese";
const AUTO_MODE_ID: &str = "com.cyberneura.inputmethod.Kayoiji.Auto";
/// `setValue:forTag:client:` で、入力モードの切り替えを表すタグ (`kTextServiceInputModePropertyTag`、'imod')。
const INPUT_MODE_TAG: isize = 0x696D_6F64;
/// OS が入力モードを切り替えた直後に届く「かな」キーを、切り替えの一部とみなす時間。
/// 自動切り替えでこのキーを打鍵として扱うと、切り替えた直後に「次の入力を日本語にする」指定が入ってしまう。
/// ただし、OS が選んだのが設定と違う側のモードなら、このキーで設定の側のモードに入り直す。
const MODE_SWITCH_SETTLE: Duration = Duration::from_millis(300);
/// 英文チェックの補助プロセスの名前。アプリの実行ファイルと同じ場所に同梱する。
const REVIEW_HELPER: &str = "english-check";
/// 英文チェックで、打鍵がこの時間止まったら、綴りの訂正候補を作り、言語モデルに訂正文と訳を頼む。
const CHECK_DELAY: Duration = Duration::from_millis(500);
/// 綴りの訂正候補を数語ずつ作る間隔。
const LOOKUP_INTERVAL: Duration = Duration::from_millis(30);
/// 綴りの検査の結果を、候補ウインドウに届けるイベント。
const SPELLING_EVENT: &str = "english-spelling";
/// 言語モデルの訂正文と訳を、候補ウインドウに届けるイベント。
const REVIEW_EVENT: &str = "english-review";
/// 言語モデルが答えられなかったことを、候補ウインドウに届けるイベント。
const REVIEW_FAILED_EVENT: &str = "english-review-failed";

/// 英文チェックで受け取っている英文に対する、綴りの検査の結果と言語モデルの答え。
#[derive(Default)]
struct EnglishCheck {
    /// 英文が変わるたびに増える番号。前の英文に対する依頼や答えを見分ける。
    id: u64,
    spelling: Option<Spelling>,
    review: Option<Review>,
}

impl EnglishCheck {
    /// Tab で英文と置き換える訂正案。言語モデルの訂正文があればそれ、無ければ綴りだけを直した文。
    /// モデルが直す所は無いと答えたら、スペルチェッカーだけが誤りとみなした単語 (固有名詞など) を
    /// 置き換えない。
    fn suggestion(&self) -> Option<String> {
        match &self.review {
            Some(review) if review.unchanged => None,
            Some(review) => Some(review.corrected.clone()),
            None => self.spelling.as_ref().and_then(|spelling| spelling.corrected.clone()),
        }
    }
}

/// 候補ウインドウに届ける、綴りの検査の結果。
#[derive(Clone, Serialize)]
struct SpellingReport {
    spelling: Option<Spelling>,
    /// 言語モデルが使えるかどうか。使えなければ、訂正文と訳は届かない。
    review_available: bool,
}

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
    check: EnglishCheck,
    /// 英文チェックの訂正文と訳を作る補助プロセス。起動できていなければ `None`。
    reviewer: Option<Reviewer>,
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
        fn set_value(&self, _value: Option<&AnyObject>, tag: isize, client: Option<&AnyObject>) {
            if tag == INPUT_MODE_TAG {
                mode_selected(client);
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
    let (output, view) = {
        let mut engine = engine.lock().expect("engine lock");
        let output = engine.set_config(config);
        (output, engine.view())
    };
    // OS の入力モードを替えると、入力先が外れる (`commit_all`) ことがある。その前に確定しておく。
    let client = RUNTIME.with(|runtime| runtime.borrow().as_ref().and_then(|runtime| runtime.client.clone()));
    if let Some(client) = client {
        apply(&client, output);
        refresh_overlay(&client, view);
    }
    sync_input_mode();
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
            check: EnglishCheck::default(),
            reviewer: None,
            _server: server,
        });
    });
    start_reviewer();
    sync_input_mode();
    true
}

/// OS の入力モードを、設定に合わせる。日本語入力と自動切り替えのうち、「かな」キーで入る側だけを
/// 有効にする。メニューバーのアイコンが、その側のものになる。
fn sync_input_mode() {
    let auto = RUNTIME.with(|runtime| {
        let runtime = runtime.borrow();
        let engine = runtime.as_ref()?.engine.lock().expect("engine lock");
        Some(engine.kana_key_mode() == InputMode::Auto)
    });
    match auto {
        Some(true) => input_source::keep_one_mode_enabled(AUTO_MODE_ID, JAPANESE_MODE_ID),
        Some(false) => input_source::keep_one_mode_enabled(JAPANESE_MODE_ID, AUTO_MODE_ID),
        None => {}
    }
}


/// 英文チェックの補助プロセスを起動する。起動できなくても、綴りの検査だけで英文チェックは使える。
/// 言語モデルが使えるかどうかは、起動した後に補助プロセスが知らせてくる。ここでは待たない。
fn start_reviewer() {
    let (Some(app), Ok(executable)) = (app_handle(), std::env::current_exe()) else {
        return;
    };
    let started = Reviewer::start(&executable.with_file_name(REVIEW_HELPER), move |event| {
        let _ = app.run_on_main_thread(move || review_event(event));
    });
    match started {
        Ok(reviewer) => RUNTIME.with(|runtime| {
            if let Some(runtime) = runtime.borrow_mut().as_mut() {
                runtime.reviewer = Some(reviewer);
            }
        }),
        Err(error) => eprintln!("kayoiji: the english check works without the language model: {error}"),
    }
}

/// 英文チェックで受け取っている英文が変わった時に、綴りを調べ直す。メインスレッドで呼ぶ。
///
/// ここでは、すでに調べた単語の訂正候補だけを付ける。新しい候補を作るのには 1 語あたり数十ミリ秒
/// かかるので、打鍵のたびには作らない。打鍵が止まったら、残りの候補を作り、訂正文と訳を頼む。
fn check_english(app: &AppHandle, text: &str) {
    let spelling = (!text.trim().is_empty()).then(|| spelling::check(text, Guesses::Known));
    let waits = spelling.is_some();
    let Some(id) = store_check(spelling.clone()) else {
        return;
    };
    report_spelling(app, spelling);
    if !waits {
        return;
    }
    let app = app.clone();
    let text = text.to_string();
    thread::spawn(move || {
        thread::sleep(CHECK_DELAY);
        let _ = app.clone().run_on_main_thread(move || complete_check(&app, id, &text, true));
    });
}

/// 英文が変わったことを記録して、新しい番号を返す。前の英文に対する依頼と答えは、番号が違うので捨てられる。
fn store_check(spelling: Option<Spelling>) -> Option<u64> {
    RUNTIME.with(|runtime| {
        let mut runtime = runtime.borrow_mut();
        let runtime = runtime.as_mut()?;
        runtime.check = EnglishCheck {
            id: runtime.check.id + 1,
            spelling,
            review: None,
        };
        Some(runtime.check.id)
    })
}

fn current_check_id() -> Option<u64> {
    RUNTIME.with(|runtime| runtime.borrow().as_ref().map(|runtime| runtime.check.id))
}

fn report_spelling(app: &AppHandle, spelling: Option<Spelling>) {
    let review_available = RUNTIME.with(|runtime| {
        let runtime = runtime.borrow();
        runtime
            .as_ref()
            .and_then(|runtime| runtime.reviewer.as_ref())
            .is_some_and(Reviewer::is_available)
    });
    let report = SpellingReport {
        spelling,
        review_available,
    };
    if let Err(error) = app.emit_to(overlay::LABEL, SPELLING_EVENT, report) {
        eprintln!("kayoiji: candidate window: {error}");
    }
}

/// 打鍵が止まった。待っている間に英文が変わっていなければ、残りの訂正候補を作り、訂正文と訳を頼む。
///
/// 訂正候補は 1 回に数語ずつ作り、残りがあれば間を置いて続ける。その間に届いた打鍵を先に処理するため。
/// 訂正文と訳を頼むのは最初の 1 回 (`ask_review`) だけ。
fn complete_check(app: &AppHandle, id: u64, text: &str, ask_review: bool) {
    if current_check_id() != Some(id) {
        return;
    }
    let spelling = spelling::check(text, Guesses::Lookup);
    let pending = spelling.pending;
    RUNTIME.with(|runtime| {
        let mut runtime = runtime.borrow_mut();
        let Some(runtime) = runtime.as_mut() else {
            return;
        };
        runtime.check.spelling = Some(spelling.clone());
        if let Some(reviewer) = runtime.reviewer.as_ref().filter(|_| ask_review) {
            reviewer.request(id, text);
        }
    });
    report_spelling(app, Some(spelling));
    if !pending {
        return;
    }
    let app = app.clone();
    let text = text.to_string();
    thread::spawn(move || {
        thread::sleep(LOOKUP_INTERVAL);
        let _ = app.clone().run_on_main_thread(move || complete_check(&app, id, &text, false));
    });
}

/// 補助プロセスからの知らせ。メインスレッドで呼ぶ。答えを待つ間に英文が変わっていたら捨てる。
fn review_event(event: Event) {
    let (id, review) = match event {
        Event::Review(review) => (Some(review.id), Some(review)),
        Event::Failed { id } => (Some(id), None),
        Event::Unavailable(reason) => {
            eprintln!("kayoiji: the language model is not available: {reason}");
            (None, None)
        }
    };
    let app = RUNTIME.with(|runtime| {
        let mut runtime = runtime.borrow_mut();
        let runtime = runtime.as_mut()?;
        if id.is_some_and(|id| id != runtime.check.id) {
            return None;
        }
        runtime.check.review = review.clone();
        Some(runtime.app.clone())
    });
    let Some(app) = app else {
        return;
    };
    let sent = match review {
        Some(review) => app.emit_to(overlay::LABEL, REVIEW_EVENT, review),
        // 答えが来ないと分かったら、候補ウインドウが待ち続けないように知らせる。
        None => app.emit_to(overlay::LABEL, REVIEW_FAILED_EVENT, ()),
    };
    if let Err(error) = sent {
        eprintln!("kayoiji: candidate window: {error}");
    }
}

fn key_press(event: &NSEvent) -> KeyPress {
    let flags = event.modifierFlags();
    let shift = flags.contains(NSEventModifierFlags::Shift);
    let modified = flags.intersects(
        NSEventModifierFlags::Control | NSEventModifierFlags::Option | NSEventModifierFlags::Command,
    );
    // Control などを押した打鍵は、キーの元の文字で見分ける (Ctrl+J)。それ以外は、Shift を反映した
    // 文字を使う。Shift + 数字の記号などは、こちらにしか入っていない。
    let first = |characters: Option<Retained<NSString>>| {
        characters.and_then(|characters| characters.to_string().chars().next())
    };
    let character = if modified {
        first(event.charactersIgnoringModifiers())
    } else {
        first(event.characters()).or_else(|| first(event.charactersIgnoringModifiers()))
    };
    KeyPress {
        key_code: event.keyCode(),
        character: character.map(|character| crate::keys::with_shift(character, shift)),
        shift,
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
        // 英文チェックの Tab は、受け取っている英文を訂正案に置き換える。
        let checking = mode_before == InputMode::English && !engine.preedit().is_empty();
        let correction = (key == Key::Tab && checking)
            .then(|| runtime.check.suggestion())
            .flatten();
        let output = match correction {
            Some(text) => engine.replace_english(&text),
            None => engine.handle(key, now),
        };
        if let Some(error) = engine.take_error() {
            eprintln!("kayoiji: could not save what was learned: {error}");
        }
        let deadline = engine.timeout_deadline().map(|deadline| deadline.saturating_sub(now));
        // 直接入力のまま押した「英数」でも選び直す。前に英字のキーボード配列を選べなかった時のやり直しになる。
        let left_for_direct =
            engine.input_mode() == InputMode::Direct && (mode_before != InputMode::Direct || key == Key::Muhenkan);
        Some((output, engine.view(), deadline, left_for_direct))
    });
    let Some((output, view, deadline, left_for_direct)) = handled else {
        return false;
    };
    let consumed = output.consumed;
    apply(client, output);
    refresh_overlay(client, view);
    if let Some(delay) = deadline {
        schedule_timeout(Duration::from_millis(delay + 1));
    }
    if left_for_direct {
        // 直接入力は、OS の英字のキーボード配列に任せる。メニューバーのアイコンが英字のものになる。
        input_source::select_ascii_layout();
    }
    consumed
}

/// 今届いた「かな」キーが、OS が済ませた入力モードの切り替えの一部かどうか。
///
/// OS は、日本語入力と自動切り替えのどちらが設定されているかを知らない。「かな」キーで OS が選んだのが
/// 設定と違う側のモードなら、切り替えはまだ済んでいない。エンジンに渡して、設定の側のモードに入り直す。
fn settles_mode_switch() -> bool {
    RUNTIME.with(|runtime| {
        let mut runtime = runtime.borrow_mut();
        let Some(runtime) = runtime.as_mut() else {
            return false;
        };
        let just_switched = runtime
            .mode_switched_at
            .is_some_and(|switched| switched.elapsed() < MODE_SWITCH_SETTLE);
        let mut engine = runtime.engine.lock().expect("engine lock");
        let settles = just_switched && engine.input_mode() == engine.kana_key_mode();
        if settles {
            // 切り替えの一部とみなすのは、切り替えの後の最初の「かな」だけ。続けて押した 2 度目まで
            // 吸い込むと、2 度押しがエンジンに届かない。
            runtime.mode_switched_at = None;
            // エンジンに渡さない打鍵も、「かな」の 2 度押しの 1 度目には数える。数えないと、英字の
            // キーボード配列から英文チェックに入るのに、「かな」を 3 度押すことになる。
            engine.mark_henkan(runtime.started.elapsed().as_millis() as u64);
        }
        settles
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
    let anchor = if overlay::is_hidden(&view) {
        None
    } else {
        preedit_rect(client)
    };
    // 英文チェックを抜けた時も呼ぶ。空の英文として記録し直すと、前の英文への依頼と答えが捨てられる。
    let english = if view.mode == Mode::Checking {
        view.reading.clone()
    } else {
        String::new()
    };
    if let Err(error) = overlay::update(&app, view, anchor) {
        eprintln!("kayoiji: candidate window: {error}");
    }
    check_english(&app, &english);
}

/// 表示中の未確定文字列を、そのまま確定する。
fn commit_all(client: Option<&AnyObject>) {
    let committed = RUNTIME.with(|runtime| {
        let mut runtime = runtime.borrow_mut();
        let runtime = runtime.as_mut()?;
        runtime.client = None;
        runtime.commit_count += 1;
        // 確定した英文に対する訂正案を残さない。残すと、次の Tab がその訂正案に使われて消える。
        runtime.check = EnglishCheck {
            id: runtime.check.id + 1,
            ..EnglishCheck::default()
        };
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
            runtime.last_view = view.clone();
        }
    });
    if let Err(error) = overlay::hide(&app, view) {
        eprintln!("kayoiji: candidate window: {error}");
    }
}

/// OS が切り替えた入力モードを、エンジンに反映する。
fn mode_selected(client: Option<&AnyObject>) {
    let switched = RUNTIME.with(|runtime| {
        let mut runtime = runtime.borrow_mut();
        let runtime = runtime.as_mut()?;
        let mut engine = runtime.engine.lock().expect("engine lock");
        // 有効な入力モードは 1 つなので、この通知は「この入力メソッドが選ばれた」か「入力先が替わった」を表す。
        // 直接入力のまま選ばれた時 (「英数」で英字のキーボード配列に切り替え、「かな」で戻って来た時) は、
        // 「かな」で入るモードにする。それ以外のモードは、そのまま続ける。
        if engine.input_mode() != InputMode::Direct {
            return None;
        }
        runtime.mode_switched_at = Some(Instant::now());
        let mode = engine.kana_key_mode();
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
    fn the_info_plist_declares_the_two_input_modes() {
        // Arrange
        let info_plist = include_str!("../Info.plist");

        // Act
        let modes = info_plist.matches("<key>tsInputModeScriptKey</key>").count();
        let declared = |identifier: &str| {
            info_plist
                .split_once(&format!("<key>{identifier}</key>"))
                .and_then(|(_, mode)| mode.split_once("<key>TISInputSourceID</key>"))
                .and_then(|(_, rest)| rest.split_once("<string>"))
                .and_then(|(_, rest)| rest.split_once("</string>"))
                .map(|(source_id, _)| source_id.to_string())
        };

        // Assert
        // 入力モードは、アイコンの違う日本語入力と自動切り替えの 2 つ。ID が食い違うと、有効にする側を
        // 入れ替えられない。
        assert_eq!(modes, 2);
        assert_eq!(declared(JAPANESE_MODE_ID).as_deref(), Some(JAPANESE_MODE_ID));
        assert_eq!(declared(AUTO_MODE_ID).as_deref(), Some(AUTO_MODE_ID));
    }

    fn review(corrected: &str, unchanged: bool) -> Review {
        Review {
            id: 1,
            corrected: corrected.to_string(),
            unchanged,
            translation: String::new(),
        }
    }

    fn spelling_fix(corrected: &str) -> Spelling {
        Spelling {
            misspellings: Vec::new(),
            corrected: Some(corrected.to_string()),
            pending: false,
        }
    }

    #[test]
    fn the_spelling_fix_is_suggested_until_the_language_model_answers() {
        let check = EnglishCheck {
            spelling: Some(spelling_fix("Please send me the file")),
            ..EnglishCheck::default()
        };
        assert_eq!(check.suggestion().as_deref(), Some("Please send me the file"));
    }

    #[test]
    fn the_correction_of_the_language_model_is_preferred() {
        let check = EnglishCheck {
            spelling: Some(spelling_fix("I has a pen")),
            review: Some(review("I have a pen.", false)),
            ..EnglishCheck::default()
        };
        assert_eq!(check.suggestion().as_deref(), Some("I have a pen."));
    }

    #[test]
    fn nothing_is_suggested_when_the_language_model_finds_nothing_to_fix() {
        // 固有名詞のように、スペルチェッカーだけが誤りとみなした単語を置き換えない。
        let check = EnglishCheck {
            spelling: Some(spelling_fix("Kayoiji is nice")),
            review: Some(review("Kayoji is nice", true)),
            ..EnglishCheck::default()
        };
        assert_eq!(check.suggestion(), None);
    }

}
