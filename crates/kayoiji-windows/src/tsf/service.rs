//! テキストサービス本体。打鍵をエンジンに渡し、その結果を入力先の文書 (TSF のコンテキスト) に反映する。
//!
//! TSF のテキストサービスは、入力先のスレッドごとに 1 つ作られる。エンジンもスレッドごとに持ち、辞書は
//! ファイルを mmap するので、同じプロセスの中で読み込みが重なっても、メモリは OS が共有する。
//!
//! 入力先の文書は、編集セッション (`ITfEditSession`) の中でしか書き換えられない。TSF は、こちらが
//! 呼び出した編集の途中で、こちらのメソッド (`OnCompositionTerminated` など) を呼び返すことがあるので、
//! 状態の `RefCell` を借りたまま TSF を呼ばない。
//!
//! 打鍵は OnTestKeyDown でエンジンに渡して、使うかどうかを決める。IMM32 を経由するアプリは、OnTestKeyDown で
//! 使うと答えたキーを VK_PROCESSKEY に置き換えるので、続く OnKeyDown で使わないと答えても、キーはアプリに届かない。

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Instant;

use kayoiji_core::engine::{Engine, InputMode, Key, Output};
use kayoiji_core::learning::Learning;
use kayoiji_core::resources::{load_rules, open_engine};
use kayoiji_core::settings::Settings;
use windows::Win32::Foundation::{LPARAM, MAX_PATH, RECT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleFileNameW;
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, GetKeyboardState, MapVirtualKeyW, SendInput, ToUnicode, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT,
    KEYBD_EVENT_FLAGS, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, MAPVK_VK_TO_VSC, VIRTUAL_KEY,
};
use windows::Win32::UI::TextServices::{
    ITfComposition, ITfCompositionSink, ITfCompositionSink_Impl, ITfCompartmentEventSink, ITfCompartmentEventSink_Impl,
    ITfCompartmentMgr, ITfContext, ITfContextComposition, ITfDocumentMgr, ITfEditSession, ITfEditSession_Impl,
    ITfInsertAtSelection, ITfKeyEventSink, ITfKeyEventSink_Impl, ITfKeystrokeMgr, ITfRange, ITfSource,
    ITfTextInputProcessorEx_Impl, ITfTextInputProcessor_Impl, ITfThreadMgr, ITfThreadMgrEventSink,
    ITfThreadMgrEventSink_Impl, ITfTextInputProcessorEx, GUID_COMPARTMENT_KEYBOARD_INPUTMODE_CONVERSION,
    GUID_COMPARTMENT_KEYBOARD_OPENCLOSE, TF_AE_NONE, TF_ANCHOR_END, TF_CONTEXT_EDIT_CONTEXT_FLAGS,
    TF_CONVERSIONMODE_FULLSHAPE, TF_CONVERSIONMODE_NATIVE, TF_ES_ASYNCDONTCARE, TF_ES_READ, TF_ES_READWRITE, TF_ES_SYNC,
    TF_IAS_QUERYONLY, TF_SELECTION, TF_SELECTIONSTYLE,
};
use windows::Win32::UI::WindowsAndMessaging::GetMessageExtraInfo;
use windows_core::{implement, IUnknownImpl, Interface, Ref, BOOL, GUID};

use crate::keys::{self, KeyStroke, Mapped};
use crate::panel::panel;

use super::module;
use super::window::CandidateWindow;

/// 確定してからキーを通すために送り直したキーの目印 (`dwExtraInfo`)。送り直したキーは、エンジンに渡さずに通す。
const RESENT: usize = 0x4B59_4A49;

/// 有効になっている間の状態。
struct Active {
    thread_manager: ITfThreadMgr,
    client_id: u32,
    thread_sink: Option<u32>,
    compartment_sink: Option<u32>,
    /// 辞書が開けない時は `None`。その時は打鍵をすべて入力先に通す。
    engine: Option<Engine>,
    started: Instant,
    /// 最後に打鍵を受けた文書。打鍵の間隔のタイマーが切れた時に、そこへ反映する。
    context: Option<ITfContext>,
    window: Option<CandidateWindow>,
    /// 入力モードの compartment を、自分で書き換えている最中かどうか。その変更の知らせは無視する。
    setting_compartment: bool,
    /// OnTestKeyDown でエンジンに渡した打鍵の結果。続く OnKeyDown で文書に反映する。
    decided: Option<Decision>,
}

/// 打鍵をエンジンに渡した結果。
struct Decision {
    wparam: usize,
    lparam: isize,
    /// 入力先に反映する出力。キーを入力先に通す時は `None`。
    output: Option<Output>,
    mode_changed: bool,
    /// 候補ウインドウの表示内容が変わったかどうか。文書に書かない打鍵でも、候補ウインドウは出し直す。
    view_changed: bool,
    /// 確定を書いた後に、入力先に通すために送り直すキー。
    resend: Option<u16>,
}

impl Decision {
    fn eaten(&self) -> bool {
        self.output.is_some()
    }

    fn is_for(&self, wparam: WPARAM, lparam: LPARAM) -> bool {
        self.wparam == wparam.0 && self.lparam == lparam.0
    }
}

/// 文書への 1 回の書き込み。
enum Edit {
    Output(Output),
    /// 表示している未確定文字列を、書き直さずに終わらせる (入力先が切り替わる時)。
    Finish,
    /// 文書は変えずに、候補ウインドウだけを今の表示内容にする (絵文字モードを抜けてキーを通した時など)。
    Refresh,
}

#[implement(ITfTextInputProcessorEx, ITfKeyEventSink, ITfCompositionSink, ITfThreadMgrEventSink, ITfCompartmentEventSink)]
pub struct TextService {
    active: RefCell<Option<Active>>,
    /// 今の未確定文字列。無効にされた後に動く編集セッションでも使うので、`Active` の外に持つ。
    composition: RefCell<Option<ITfComposition>>,
    /// 文書に書いていない書き込み。編集セッションは、TSF の都合で後から動くことも、後に頼んだものが先に
    /// 動くこともある (同期のものは、待っている非同期のものより先に動く)。書き込みは順に積み、動いた
    /// 編集セッションが前から順に書く。
    queue: RefCell<VecDeque<(ITfContext, Edit)>>,
}

impl TextService {
    pub fn new() -> Self {
        Self {
            active: RefCell::new(None),
            composition: RefCell::new(None),
            queue: RefCell::new(VecDeque::new()),
        }
    }
}

/// 利用者のファイル (設定、規則、学習) を置くディレクトリ。`%APPDATA%\Kayoiji`。
fn user_dir() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|appdata| PathBuf::from(appdata).join("Kayoiji"))
}

/// 辞書のディレクトリ。インストーラーが DLL の隣の `data` に置く。
fn data_dir() -> Option<PathBuf> {
    let mut buffer = vec![0u16; MAX_PATH as usize * 4];
    // SAFETY: `buffer` は書き込める長さを持つ。
    let length = unsafe { GetModuleFileNameW(Some(module().into()), &mut buffer) } as usize;
    if length == 0 || length >= buffer.len() {
        return None;
    }
    let path = PathBuf::from(String::from_utf16_lossy(&buffer[..length]));
    Some(path.parent()?.join("data"))
}

/// エンジンを開く。利用者のファイルが読めない時は既定の内容で開く (入力メソッドが起動しないと、何も打てなくなる)。
fn open() -> Option<Engine> {
    let user = user_dir();
    let settings = user
        .as_ref()
        .and_then(|user| Settings::load(&user.join("settings.toml")).ok())
        .unwrap_or_default();
    let mut config = settings.engine_config();
    // 英文チェックは、macOS の言語モデルとスペルチェッカーで作る。
    config.english_check = false;
    let rules = match &user {
        Some(user) => load_rules(&user.join("rules.toml")).0,
        None => kayoiji_core::rules::RuleSet::standard(),
    };
    let learning = match &user {
        Some(user) => Learning::load_or_set_aside(&user.join("learning.json")).0,
        None => Learning::in_memory(),
    };
    open_engine(&data_dir()?, rules, learning, config).ok()
}

/// 押されたキーを、今のキーボード配列の文字と修飾キーと合わせて読む。
fn key_stroke(wparam: WPARAM, lparam: LPARAM) -> KeyStroke {
    let vk = wparam.0 as u16;
    // SAFETY: キーの状態を読むだけ。
    let pressed = |key: u16| unsafe { GetKeyState(i32::from(key)) } < 0;
    let mut stroke = KeyStroke {
        shift: pressed(keys::VK_SHIFT),
        control: pressed(keys::VK_CONTROL),
        alt: pressed(keys::VK_MENU),
        // lParam の 30 bit 目は、このキーが前から押されていたかどうか。
        repeat: (lparam.0 >> 30) & 1 == 1,
        ..KeyStroke::new(vk)
    };
    let mut state = [0u8; 256];
    // SAFETY: `state` は 256 バイト。
    if unsafe { GetKeyboardState(&mut state) }.is_ok() {
        // Ctrl を付けたままだと制御文字になるので、Ctrl を外した文字を読む。
        state[usize::from(keys::VK_CONTROL)] = 0;
        state[0xA2] = 0;
        state[0xA3] = 0;
        let mut buffer = [0u16; 8];
        // SAFETY: 仮想キーから走査コードを引き、キーボードの状態を変えずに文字を読む (flags の 4)。
        let count = unsafe {
            let scan = MapVirtualKeyW(u32::from(vk), MAPVK_VK_TO_VSC);
            ToUnicode(u32::from(vk), scan, Some(&state), &mut buffer, 4)
        };
        if count == 1 {
            stroke.character = char::from_u32(u32::from(buffer[0]));
        }
    }
    stroke
}

impl TextService_Impl {
    fn now(active: &Active) -> u64 {
        active.started.elapsed().as_millis() as u64
    }

    fn client_id(&self) -> Option<u32> {
        self.active.borrow().as_ref().map(|active| active.client_id)
    }

    /// エンジンの入力モードを、OS の入力モードの表示 (タスクバーの「あ」「A」) に合わせる。
    fn sync_compartment(&self) {
        let (thread_manager, client_id, open) = {
            let mut active = self.active.borrow_mut();
            let Some(active) = active.as_mut() else {
                return;
            };
            let Some(engine) = &active.engine else {
                return;
            };
            active.setting_compartment = true;
            (active.thread_manager.clone(), active.client_id, engine.input_mode() != InputMode::Direct)
        };
        // SAFETY: スレッドの compartment に値を書く。
        unsafe {
            if let Ok(manager) = thread_manager.cast::<ITfCompartmentMgr>() {
                if let Ok(compartment) = manager.GetCompartment(&GUID_COMPARTMENT_KEYBOARD_OPENCLOSE) {
                    let _ = compartment.SetValue(client_id, &VARIANT::from(i32::from(open)));
                }
                if let Ok(compartment) = manager.GetCompartment(&GUID_COMPARTMENT_KEYBOARD_INPUTMODE_CONVERSION) {
                    let mode = if open { TF_CONVERSIONMODE_NATIVE | TF_CONVERSIONMODE_FULLSHAPE } else { 0 };
                    let _ = compartment.SetValue(client_id, &VARIANT::from(mode as i32));
                }
            }
        }
        if let Some(active) = self.active.borrow_mut().as_mut() {
            active.setting_compartment = false;
        }
    }

    /// 書き込みを積み、`context` の編集セッションを頼む。打鍵の受け口の中なら `sync` にできる。
    fn apply(&self, context: &ITfContext, edit: Edit, sync: bool) {
        if self.client_id().is_none() {
            return;
        }
        self.queue.borrow_mut().push_back((context.clone(), edit));
        self.request_drain(context, sync);
    }

    /// 積んである書き込みを書く編集セッションを、`context` に頼む。
    fn request_drain(&self, context: &ITfContext, sync: bool) {
        let Some(client_id) = self.client_id() else {
            return;
        };
        let service = self.to_object();
        let target = context.clone();
        let session: ITfEditSession = EditSession {
            action: Box::new(move |cookie| service.drain(cookie, &target)),
        }
        .into();
        let request = |flags: TF_CONTEXT_EDIT_CONTEXT_FLAGS| {
            // SAFETY: 読み書きの編集セッションを頼む。
            unsafe { context.RequestEditSession(client_id, &session, flags | TF_ES_READWRITE) }
                .is_ok_and(|result| result.is_ok())
        };
        if !(sync && request(TF_ES_SYNC)) {
            request(TF_ES_ASYNCDONTCARE);
        }
    }

    /// 積んである書き込みのうち、`context` の分を前から順に書く。ほかの文書の分が先頭に来たら、その文書の
    /// 編集セッションに任せる。
    fn drain(&self, cookie: u32, context: &ITfContext) -> windows_core::Result<()> {
        loop {
            let next = {
                let mut queue = self.queue.borrow_mut();
                match queue.front() {
                    Some((target, _)) if target == context => queue.pop_front(),
                    _ => None,
                }
            };
            let Some((_, edit)) = next else {
                break;
            };
            match edit {
                Edit::Output(output) => self.edit(cookie, context, &output)?,
                Edit::Finish => self.finish(cookie, context)?,
                // SAFETY: 編集セッションの中で、未確定文字列かキャレットの位置を読む。
                Edit::Refresh => unsafe { self.refresh(cookie, context) },
            }
        }
        // ほかの文書の分が先頭に残った。その文書のために頼んだ編集セッションが先に動いて、何も書かずに
        // 終わっていることがあるので、もう一度頼む。
        let blocked = self.queue.borrow().front().map(|(target, _)| target.clone());
        if let Some(target) = blocked.filter(|target| target != context) {
            self.request_drain(&target, false);
        }
        Ok(())
    }

    fn finish(&self, cookie: u32, context: &ITfContext) -> windows_core::Result<()> {
        let Some(composition) = self.composition.borrow_mut().take() else {
            return Ok(());
        };
        // SAFETY: 編集セッションの中で、未確定文字列の後ろにキャレットを置いてから終わらせる。
        unsafe {
            if let Ok(range) = composition.GetRange() {
                range.Collapse(cookie, TF_ANCHOR_END)?;
                set_caret(cookie, context, &range)?;
            }
            composition.EndComposition(cookie)
        }
    }

    /// 編集セッションの中で、確定と未確定文字列を書き、候補ウインドウを動かす。
    fn edit(&self, cookie: u32, context: &ITfContext, output: &Output) -> windows_core::Result<()> {
        let mut composition = self.composition.borrow_mut().take();
        // SAFETY: 編集セッションの中なので、`cookie` で文書を読み書きできる。
        unsafe {
            if !output.reread.is_empty() && composition.is_none() {
                remove_before_caret(cookie, context, &output.reread)?;
            }
            if !output.commit.is_empty() || output.preedit.is_empty() {
                let range = match &composition {
                    Some(composition) => composition.GetRange()?,
                    None => context.cast::<ITfInsertAtSelection>()?.InsertTextAtSelection(cookie, TF_IAS_QUERYONLY, &[])?,
                };
                let text: Vec<u16> = output.commit.encode_utf16().collect();
                if composition.is_some() || !text.is_empty() {
                    range.SetText(cookie, 0, &text)?;
                    range.Collapse(cookie, TF_ANCHOR_END)?;
                    set_caret(cookie, context, &range)?;
                }
                if let Some(ended) = composition.take() {
                    ended.EndComposition(cookie)?;
                }
            }
            let mut anchor = None;
            if !output.preedit.is_empty() {
                let started = match composition {
                    Some(composition) => composition,
                    None => {
                        let range =
                            context.cast::<ITfInsertAtSelection>()?.InsertTextAtSelection(cookie, TF_IAS_QUERYONLY, &[])?;
                        let sink: ITfCompositionSink = self.to_object().to_interface();
                        context.cast::<ITfContextComposition>()?.StartComposition(cookie, &range, &sink)?
                    }
                };
                let range = started.GetRange()?;
                let text: Vec<u16> = output.preedit.encode_utf16().collect();
                range.SetText(cookie, 0, &text)?;
                let caret = range.Clone()?;
                caret.Collapse(cookie, TF_ANCHOR_END)?;
                set_caret(cookie, context, &caret)?;
                let mut rect = RECT::default();
                let mut clipped = BOOL::default();
                if context.GetActiveView()?.GetTextExt(cookie, &range, &mut rect, &mut clipped).is_ok() {
                    anchor = Some(rect);
                }
                *self.composition.borrow_mut() = Some(started);
            }
            if anchor.is_none() {
                anchor = caret_rect(cookie, context);
            }
            self.refresh_window(anchor);
        }
        Ok(())
    }

    /// 未確定文字列 (無ければキャレット) の位置に、候補ウインドウを出し直す。
    unsafe fn refresh(&self, cookie: u32, context: &ITfContext) {
        let composition = self.composition.borrow().clone();
        // SAFETY: 呼び出し元は編集セッションの中。
        let anchor = unsafe {
            composition
                .and_then(|composition| composition.GetRange().ok())
                .and_then(|range| {
                    let mut rect = RECT::default();
                    let mut clipped = BOOL::default();
                    let view = context.GetActiveView().ok()?;
                    view.GetTextExt(cookie, &range, &mut rect, &mut clipped).ok()?;
                    Some(rect)
                })
                .or_else(|| caret_rect(cookie, context))
        };
        self.refresh_window(anchor);
    }

    fn refresh_window(&self, anchor: Option<RECT>) {
        let active = self.active.borrow();
        let Some(active) = active.as_ref() else {
            return;
        };
        let (Some(engine), Some(window)) = (&active.engine, &active.window) else {
            return;
        };
        window.show(panel(&engine.view()), anchor);
    }

    /// 打鍵の間隔のタイマーを、エンジンが待っている時刻に合わせる。
    fn schedule_timeout(&self) {
        let active = self.active.borrow();
        let Some(active) = active.as_ref() else {
            return;
        };
        let (Some(engine), Some(window)) = (&active.engine, &active.window) else {
            return;
        };
        let Some(deadline) = engine.timeout_deadline() else {
            window.cancel_timer();
            return;
        };
        let delay = deadline.saturating_sub(Self::now(active)) + 1;
        let service = self.to_object();
        window.set_timer(delay as u32, Box::new(move || service.timed_out()));
    }

    fn timed_out(&self) {
        let (output, context) = {
            let mut active = self.active.borrow_mut();
            let Some(active) = active.as_mut() else {
                return;
            };
            let now = Self::now(active);
            let (Some(engine), Some(context)) = (active.engine.as_mut(), active.context.clone()) else {
                return;
            };
            (engine.handle_timeout(now), context)
        };
        self.apply(&context, Edit::Output(output), false);
        self.schedule_timeout();
    }

    /// 打鍵をエンジンに渡して、使うかどうかを決める。使う打鍵の結果は、続く OnKeyDown のために取っておく。
    fn decide(&self, context: &ITfContext, wparam: WPARAM, lparam: LPARAM) -> bool {
        // SAFETY: 今の打鍵のメッセージに付いた値を読む。
        if unsafe { GetMessageExtraInfo() }.0 as usize == RESENT {
            return false;
        }
        let earlier = {
            let mut active = self.active.borrow_mut();
            let Some(active) = active.as_mut() else {
                return false;
            };
            // TSF は同じ打鍵の OnTestKeyDown を 2 度呼ぶことがある。
            if let Some(decision) = active.decided.as_ref().filter(|decision| decision.is_for(wparam, lparam)) {
                return decision.eaten();
            }
            active.decided.take()
        };
        // 使うと答えたのに OnKeyDown が来なかった打鍵。結果を捨てると入力が欠けるので、ここで反映する。
        if let Some(earlier) = earlier {
            self.perform(context, earlier);
        }
        let stroke = key_stroke(wparam, lparam);
        let (key, target) = {
            let active = self.active.borrow();
            let Some(engine) = active.as_ref().and_then(|active| active.engine.as_ref()) else {
                return false;
            };
            let key = match keys::translate(stroke, engine.view().mode) {
                Mapped::Modifier => return false,
                Mapped::Toggle if engine.input_mode() == InputMode::Direct => Key::Henkan,
                Mapped::Toggle => Key::Muhenkan,
                Mapped::Engine(key) => key,
            };
            let target = if key == Key::ForceJapanese { engine.reread_target() } else { String::new() };
            (key, target)
        };
        // 確定済みの単語の読み直しは、その単語がキャレットの直前にある時だけ行う。無ければエンジンに渡さない。
        // 渡すと、単語を日本語として覚えた上に、未確定文字列がその時のキャレットの位置に入る。
        if !target.is_empty() && !self.precedes_caret(context, &target) {
            return false;
        }
        let decision = {
            let mut active = self.active.borrow_mut();
            let Some(active) = active.as_mut() else {
                return false;
            };
            let now = Self::now(active);
            active.context = Some(context.clone());
            let Some(engine) = active.engine.as_mut() else {
                return false;
            };
            let was_direct = engine.input_mode() == InputMode::Direct;
            let preedit = engine.preedit();
            let view = engine.view();
            let mut output = engine.handle(key, now);
            let mode_changed = was_direct != (engine.input_mode() == InputMode::Direct);
            let view_changed = engine.view() != view;
            let writes = !output.commit.is_empty() || !output.reread.is_empty() || output.preedit != preedit;
            let mut resend = None;
            let output = if output.consumed {
                Some(output)
            } else if !writes {
                None
            } else {
                // 確定してからキーを入力先に通す。文字と空白は確定する文字列の後ろに足し、ほかのキーは
                // 確定を書いた後に送り直す。
                match key {
                    Key::Space => output.commit.push(' '),
                    Key::Char(c) => output.commit.push(c),
                    _ => resend = Some(stroke.vk),
                }
                output.consumed = true;
                Some(output)
            };
            Decision {
                wparam: wparam.0,
                lparam: lparam.0,
                output,
                mode_changed,
                view_changed,
                resend,
            }
        };
        if !decision.eaten() {
            if decision.view_changed {
                self.apply(context, Edit::Refresh, true);
            }
            if decision.mode_changed {
                self.sync_compartment();
            }
            self.schedule_timeout();
            return false;
        }
        if let Some(active) = self.active.borrow_mut().as_mut() {
            active.decided = Some(decision);
        }
        true
    }

    fn key_down(&self, context: &ITfContext, wparam: WPARAM, lparam: LPARAM) -> bool {
        let take = || {
            self.active.borrow_mut().as_mut().and_then(|active| {
                active
                    .decided
                    .take_if(|decision| decision.is_for(wparam, lparam))
            })
        };
        let decision = match take() {
            Some(decision) => decision,
            None if self.decide(context, wparam, lparam) => match take() {
                Some(decision) => decision,
                None => return false,
            },
            None => return false,
        };
        self.perform(context, decision);
        true
    }

    fn perform(&self, context: &ITfContext, decision: Decision) {
        if let Some(output) = decision.output {
            self.apply(context, Edit::Output(output), true);
        }
        if decision.mode_changed {
            self.sync_compartment();
        }
        self.schedule_timeout();
        if let Some(vk) = decision.resend {
            resend(vk);
        }
    }

    /// `text` がキャレットの直前にあり、選択範囲が無いかどうか。読み取りの編集セッションで同期して調べる。
    fn precedes_caret(&self, context: &ITfContext, text: &str) -> bool {
        let Some(client_id) = self.client_id() else {
            return false;
        };
        let found = Rc::new(Cell::new(false));
        let result = Rc::clone(&found);
        let target = context.clone();
        let expected: Vec<u16> = text.encode_utf16().collect();
        let session: ITfEditSession = EditSession {
            action: Box::new(move |cookie| {
                // SAFETY: 読み取りの編集セッションの中で調べる。
                result.set(unsafe { text_before_caret(cookie, &target, expected.len()) }.is_ok_and(|text| text == expected));
                Ok(())
            }),
        }
        .into();
        // SAFETY: 打鍵の受け口の中なので、同期の読み取りの編集セッションを頼める。
        let requested = unsafe { context.RequestEditSession(client_id, &session, TF_ES_SYNC | TF_ES_READ) };
        requested.is_ok_and(|result| result.is_ok()) && found.get()
    }

    /// 未確定文字列を表示のとおりに確定する。入力先が切り替わる時と、無効にされる時に呼ぶ。
    /// 入力先の文書には、表示している文字列がそのまま入っているので、書き直さずに未確定文字列を終わらせる。
    fn commit_all(&self) {
        let context = {
            let mut active = self.active.borrow_mut();
            let Some(active) = active.as_mut() else {
                return;
            };
            active.decided = None;
            if let Some(engine) = active.engine.as_mut() {
                engine.commit_composition();
            }
            if let Some(window) = &active.window {
                window.hide();
                window.cancel_timer();
            }
            active.context.clone()
        };
        // 未確定文字列がまだ無くても、積んである書き込みがそれを作ることがあるので、その後ろで終わらせる。
        // 借りたまま apply を呼ばない。編集セッションが同期して動くと、finish が同じ RefCell を借りる。
        let composing = self.composition.borrow().is_some() || !self.queue.borrow().is_empty();
        if let (Some(context), true) = (context, composing) {
            self.apply(&context, Edit::Finish, false);
        }
    }

    fn advise(&self, thread_manager: &ITfThreadMgr, client_id: u32) -> windows_core::Result<(u32, u32)> {
        let service = self.to_object();
        let key_sink: ITfKeyEventSink = service.to_interface();
        let thread_sink: ITfThreadMgrEventSink = service.to_interface();
        let compartment_sink: ITfCompartmentEventSink = service.to_interface();
        // SAFETY: 有効にされたスレッドのマネージャーに、このオブジェクトの受け口を登録する。
        unsafe {
            thread_manager.cast::<ITfKeystrokeMgr>()?.AdviseKeyEventSink(client_id, &key_sink, true)?;
            let thread_cookie = thread_manager.cast::<ITfSource>()?.AdviseSink(&ITfThreadMgrEventSink::IID, &thread_sink)?;
            let compartment = thread_manager
                .cast::<ITfCompartmentMgr>()?
                .GetCompartment(&GUID_COMPARTMENT_KEYBOARD_OPENCLOSE)?;
            let compartment_cookie =
                compartment.cast::<ITfSource>()?.AdviseSink(&ITfCompartmentEventSink::IID, &compartment_sink)?;
            Ok((thread_cookie, compartment_cookie))
        }
    }
}

/// 確定の後に、キーを入力先に送り直す。目印を付けて、エンジンには渡さない。
fn resend(vk: u16) {
    let extended = matches!(vk, 0x21..=0x28 | 0x2D | 0x2E);
    let event = |up: bool| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(vk),
                wScan: 0,
                dwFlags: KEYBD_EVENT_FLAGS(
                    if extended { KEYEVENTF_EXTENDEDKEY.0 } else { 0 } | if up { KEYEVENTF_KEYUP.0 } else { 0 },
                ),
                time: 0,
                dwExtraInfo: RESENT,
            },
        },
    };
    // SAFETY: 押して離すキーの入力を 2 つ送る。
    unsafe { SendInput(&[event(false), event(true)], std::mem::size_of::<INPUT>() as i32) };
}

/// 選択範囲が無い時の、キャレットの直前の `length` 単位 (UTF-16) の文字列。選択範囲がある時はエラー。
unsafe fn text_before_caret(cookie: u32, context: &ITfContext, length: usize) -> windows_core::Result<Vec<u16>> {
    // SAFETY: 呼び出し元は編集セッションの中。
    unsafe {
        let range = context.cast::<ITfInsertAtSelection>()?.InsertTextAtSelection(cookie, TF_IAS_QUERYONLY, &[])?;
        if !range.IsEmpty(cookie)?.as_bool() {
            return Err(windows::Win32::Foundation::E_FAIL.into());
        }
        let mut moved = 0;
        range.ShiftStart(cookie, -(length as i32), &mut moved, std::ptr::null())?;
        let mut buffer = vec![0u16; length];
        let mut fetched = 0;
        range.GetText(cookie, 0, &mut buffer, &mut fetched)?;
        buffer.truncate(fetched as usize);
        Ok(buffer)
    }
}

/// 確定済みの文字列のうち、キャレットの直前の `text` を消す (Ctrl+J の読み直し)。直前の文字列が
/// `text` と違う時と、選択範囲がある時は、利用者がキャレットを動かしたので、何もしない。
unsafe fn remove_before_caret(cookie: u32, context: &ITfContext, text: &str) -> windows_core::Result<()> {
    let expected: Vec<u16> = text.encode_utf16().collect();
    // SAFETY: 呼び出し元は編集セッションの中。
    unsafe {
        if text_before_caret(cookie, context, expected.len()).is_ok_and(|found| found == expected) {
            let range = context.cast::<ITfInsertAtSelection>()?.InsertTextAtSelection(cookie, TF_IAS_QUERYONLY, &[])?;
            let mut moved = 0;
            range.ShiftStart(cookie, -(expected.len() as i32), &mut moved, std::ptr::null())?;
            range.SetText(cookie, 0, &[])?;
        }
    }
    Ok(())
}

unsafe fn set_caret(cookie: u32, context: &ITfContext, range: &ITfRange) -> windows_core::Result<()> {
    let selection = TF_SELECTION {
        range: std::mem::ManuallyDrop::new(Some(range.clone())),
        style: TF_SELECTIONSTYLE {
            ase: TF_AE_NONE,
            fInterimChar: false.into(),
        },
    };
    // SAFETY: 呼び出し元は編集セッションの中。
    let result = unsafe { context.SetSelection(cookie, std::slice::from_ref(&selection)) };
    drop(std::mem::ManuallyDrop::into_inner(selection.range));
    result
}

unsafe fn caret_rect(cookie: u32, context: &ITfContext) -> Option<RECT> {
    // SAFETY: 呼び出し元は編集セッションの中。
    unsafe {
        let range = context
            .cast::<ITfInsertAtSelection>()
            .ok()?
            .InsertTextAtSelection(cookie, TF_IAS_QUERYONLY, &[])
            .ok()?;
        let mut rect = RECT::default();
        let mut clipped = BOOL::default();
        context.GetActiveView().ok()?.GetTextExt(cookie, &range, &mut rect, &mut clipped).ok()?;
        Some(rect)
    }
}

impl ITfTextInputProcessor_Impl for TextService_Impl {
    fn Activate(&self, thread_manager: Ref<ITfThreadMgr>, client_id: u32) -> windows_core::Result<()> {
        self.ActivateEx(thread_manager, client_id, 0)
    }

    fn Deactivate(&self) -> windows_core::Result<()> {
        self.commit_all();
        let Some(active) = self.active.borrow_mut().take() else {
            return Ok(());
        };
        // SAFETY: Activate で登録した受け口を外す。
        unsafe {
            if let Ok(keystrokes) = active.thread_manager.cast::<ITfKeystrokeMgr>() {
                let _ = keystrokes.UnadviseKeyEventSink(active.client_id);
            }
            if let (Some(cookie), Ok(source)) = (active.thread_sink, active.thread_manager.cast::<ITfSource>()) {
                let _ = source.UnadviseSink(cookie);
            }
            if let Some(cookie) = active.compartment_sink {
                if let Ok(compartment) = active
                    .thread_manager
                    .cast::<ITfCompartmentMgr>()
                    .and_then(|manager| manager.GetCompartment(&GUID_COMPARTMENT_KEYBOARD_OPENCLOSE))
                {
                    if let Ok(source) = compartment.cast::<ITfSource>() {
                        let _ = source.UnadviseSink(cookie);
                    }
                }
            }
        }
        Ok(())
    }
}

impl ITfTextInputProcessorEx_Impl for TextService_Impl {
    fn ActivateEx(&self, thread_manager: Ref<ITfThreadMgr>, client_id: u32, _flags: u32) -> windows_core::Result<()> {
        let thread_manager = thread_manager.ok()?.clone();
        *self.active.borrow_mut() = Some(Active {
            thread_manager: thread_manager.clone(),
            client_id,
            thread_sink: None,
            compartment_sink: None,
            engine: open(),
            started: Instant::now(),
            context: None,
            window: CandidateWindow::new(),
            setting_compartment: false,
            decided: None,
        });
        let (thread_sink, compartment_sink) = self.advise(&thread_manager, client_id)?;
        if let Some(active) = self.active.borrow_mut().as_mut() {
            active.thread_sink = Some(thread_sink);
            active.compartment_sink = Some(compartment_sink);
        }
        self.sync_compartment();
        Ok(())
    }
}

impl ITfKeyEventSink_Impl for TextService_Impl {
    /// キーボードのフォーカスを失ったら、未確定文字列を確定して候補ウインドウを隠す。候補ウインドウは
    /// 最前面に出しているので、残すとほかのアプリの上に出たままになる。
    fn OnSetFocus(&self, foreground: BOOL) -> windows_core::Result<()> {
        if !foreground.as_bool() {
            self.commit_all();
        }
        Ok(())
    }

    fn OnTestKeyDown(&self, context: Ref<ITfContext>, wparam: WPARAM, lparam: LPARAM) -> windows_core::Result<BOOL> {
        let Some(context) = context.as_ref() else {
            return Ok(false.into());
        };
        Ok(self.decide(context, wparam, lparam).into())
    }

    fn OnTestKeyUp(&self, _context: Ref<ITfContext>, _wparam: WPARAM, _lparam: LPARAM) -> windows_core::Result<BOOL> {
        Ok(false.into())
    }

    fn OnKeyDown(&self, context: Ref<ITfContext>, wparam: WPARAM, lparam: LPARAM) -> windows_core::Result<BOOL> {
        let Some(context) = context.as_ref() else {
            return Ok(false.into());
        };
        Ok(self.key_down(context, wparam, lparam).into())
    }

    fn OnKeyUp(&self, _context: Ref<ITfContext>, _wparam: WPARAM, _lparam: LPARAM) -> windows_core::Result<BOOL> {
        Ok(false.into())
    }

    fn OnPreservedKey(&self, _context: Ref<ITfContext>, _guid: *const GUID) -> windows_core::Result<BOOL> {
        Ok(false.into())
    }
}

impl ITfCompositionSink_Impl for TextService_Impl {
    /// 入力先が未確定文字列を終わらせた (クリックなど)。表示していた文字列は、入力先に確定されたまま残る。
    fn OnCompositionTerminated(&self, _cookie: u32, _composition: Ref<ITfComposition>) -> windows_core::Result<()> {
        self.composition.borrow_mut().take();
        if let Some(active) = self.active.borrow_mut().as_mut() {
            active.decided = None;
            if let Some(engine) = active.engine.as_mut() {
                engine.commit_composition();
            }
            if let Some(window) = &active.window {
                window.hide();
                window.cancel_timer();
            }
        }
        Ok(())
    }
}

impl ITfThreadMgrEventSink_Impl for TextService_Impl {
    fn OnInitDocumentMgr(&self, _document: Ref<ITfDocumentMgr>) -> windows_core::Result<()> {
        Ok(())
    }

    fn OnUninitDocumentMgr(&self, _document: Ref<ITfDocumentMgr>) -> windows_core::Result<()> {
        Ok(())
    }

    fn OnSetFocus(&self, _focus: Ref<ITfDocumentMgr>, _previous: Ref<ITfDocumentMgr>) -> windows_core::Result<()> {
        self.commit_all();
        Ok(())
    }

    fn OnPushContext(&self, _context: Ref<ITfContext>) -> windows_core::Result<()> {
        Ok(())
    }

    fn OnPopContext(&self, _context: Ref<ITfContext>) -> windows_core::Result<()> {
        Ok(())
    }
}

impl ITfCompartmentEventSink_Impl for TextService_Impl {
    /// タスクバーやキー (Alt+` など) で、OS が入力モードを切り替えた。エンジンを合わせる。
    fn OnChange(&self, guid: *const GUID) -> windows_core::Result<()> {
        // SAFETY: TSF が渡す GUID は、この呼び出しの間有効。
        if guid.is_null() || unsafe { *guid } != GUID_COMPARTMENT_KEYBOARD_OPENCLOSE {
            return Ok(());
        }
        let thread_manager = {
            let active = self.active.borrow();
            let Some(active) = active.as_ref().filter(|active| !active.setting_compartment) else {
                return Ok(());
            };
            active.thread_manager.clone()
        };
        // SAFETY: スレッドの compartment の値を読む。借用は返してから呼ぶ。
        let open = unsafe {
            thread_manager
                .cast::<ITfCompartmentMgr>()
                .and_then(|manager| manager.GetCompartment(&GUID_COMPARTMENT_KEYBOARD_OPENCLOSE))
                .and_then(|compartment| compartment.GetValue())
                .map(|value| i32::try_from(&value).unwrap_or(0) != 0)
        };
        let Ok(open) = open else {
            return Ok(());
        };
        let output = {
            let mut active = self.active.borrow_mut();
            let Some(engine) = active.as_mut().and_then(|active| active.engine.as_mut()) else {
                return Ok(());
            };
            match (open, engine.input_mode() == InputMode::Direct) {
                (true, true) => engine.set_input_mode(engine.kana_key_mode()),
                (false, false) => engine.set_input_mode(InputMode::Direct),
                _ => return Ok(()),
            }
        };
        // SAFETY: フォーカスのある文書の、いちばん上のコンテキストに反映する。
        let context = unsafe { thread_manager.GetFocus().and_then(|document| document.GetTop()) };
        if let Ok(context) = context {
            self.apply(&context, Edit::Output(output), false);
        }
        // 開閉に合わせて、変換モード (「あ」か「A」か) も書き直す。開いた時に変換モードが半角英数のまま残ると、
        // 日本語入力なのにタスクバーには「A」が出る。
        self.sync_compartment();
        Ok(())
    }
}

#[implement(ITfEditSession)]
struct EditSession {
    action: Box<dyn Fn(u32) -> windows_core::Result<()>>,
}

impl ITfEditSession_Impl for EditSession_Impl {
    fn DoEditSession(&self, cookie: u32) -> windows_core::Result<()> {
        (self.action)(cookie)
    }
}
