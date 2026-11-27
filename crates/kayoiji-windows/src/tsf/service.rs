//! テキストサービス本体。打鍵をエンジンに渡し、その結果を入力先の文書 (TSF のコンテキスト) に反映する。
//!
//! TSF のテキストサービスは、入力先のスレッドごとに 1 つ作られる。エンジンもスレッドごとに持ち、辞書は
//! ファイルを mmap するので、同じプロセスの中で読み込みが重なっても、メモリは OS が共有する。
//!
//! 入力先の文書は、編集セッション (`ITfEditSession`) の中でしか書き換えられない。TSF は、こちらが
//! 呼び出した編集の途中で、こちらのメソッド (`OnCompositionTerminated` など) を呼び返すことがあるので、
//! 状態の `RefCell` を借りたまま TSF を呼ばない。

use std::cell::RefCell;
use std::path::PathBuf;
use std::time::Instant;

use kayoiji_core::engine::{Engine, InputMode, Key, Output};
use kayoiji_core::learning::Learning;
use kayoiji_core::resources::{load_rules, open_engine};
use kayoiji_core::settings::Settings;
use windows::Win32::Foundation::{LPARAM, MAX_PATH, RECT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleFileNameW;
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, GetKeyboardState, MapVirtualKeyW, ToUnicode, MAPVK_VK_TO_VSC};
use windows::Win32::UI::TextServices::{
    ITfComposition, ITfCompositionSink, ITfCompositionSink_Impl, ITfCompartmentEventSink, ITfCompartmentEventSink_Impl,
    ITfCompartmentMgr, ITfContext, ITfContextComposition, ITfDocumentMgr, ITfEditSession, ITfEditSession_Impl,
    ITfInsertAtSelection, ITfKeyEventSink, ITfKeyEventSink_Impl, ITfKeystrokeMgr, ITfRange, ITfSource, ITfTextInputProcessor,
    ITfTextInputProcessorEx, ITfTextInputProcessorEx_Impl, ITfTextInputProcessor_Impl, ITfThreadMgr, ITfThreadMgrEventSink,
    ITfThreadMgrEventSink_Impl, GUID_COMPARTMENT_KEYBOARD_INPUTMODE_CONVERSION, GUID_COMPARTMENT_KEYBOARD_OPENCLOSE,
    TF_AE_NONE, TF_ANCHOR_END, TF_CONVERSIONMODE_FULLSHAPE, TF_CONVERSIONMODE_NATIVE, TF_ES_ASYNCDONTCARE,
    TF_ES_READWRITE, TF_IAS_QUERYONLY, TF_SELECTION, TF_SELECTIONSTYLE,
};
use windows_core::{implement, IUnknownImpl, Interface, Ref, BOOL, GUID};

use crate::keys::{self, KeyStroke, Mapped};
use crate::panel::panel;

use super::module;
use super::window::CandidateWindow;

/// 有効になっている間の状態。
struct Active {
    thread_manager: ITfThreadMgr,
    client_id: u32,
    thread_sink: Option<u32>,
    compartment_sink: Option<u32>,
    /// 辞書が開けない時は `None`。その時は打鍵をすべて入力先に通す。
    engine: Option<Engine>,
    started: Instant,
    composition: Option<ITfComposition>,
    /// 最後に打鍵を受けた文書。打鍵の間隔のタイマーが切れた時に、そこへ反映する。
    context: Option<ITfContext>,
    window: Option<CandidateWindow>,
    /// 入力モードの compartment を、自分で書き換えている最中かどうか。その変更の知らせは無視する。
    setting_compartment: bool,
}

#[implement(ITfTextInputProcessorEx, ITfKeyEventSink, ITfCompositionSink, ITfThreadMgrEventSink, ITfCompartmentEventSink)]
pub struct TextService {
    active: RefCell<Option<Active>>,
}

impl TextService {
    pub fn new() -> Self {
        Self {
            active: RefCell::new(None),
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

    /// エンジンの出力を、`context` の文書に反映する。
    fn apply(&self, context: &ITfContext, output: Output) {
        let Some(client_id) = self.active.borrow().as_ref().map(|active| active.client_id) else {
            return;
        };
        let service = self.to_object();
        let target = context.clone();
        let session: ITfEditSession = EditSession {
            action: Box::new(move |cookie| service.edit(cookie, &target, &output)),
        }
        .into();
        // SAFETY: 読み書きの編集セッションを頼む。できるなら、この呼び出しの中で同期して動く。
        let _ = unsafe { context.RequestEditSession(client_id, &session, TF_ES_ASYNCDONTCARE | TF_ES_READWRITE) };
    }

    /// 編集セッションの中で、確定と未確定文字列を書き、候補ウインドウを動かす。
    fn edit(&self, cookie: u32, context: &ITfContext, output: &Output) -> windows_core::Result<()> {
        let composition = self.active.borrow().as_ref().and_then(|active| active.composition.clone());
        // SAFETY: 編集セッションの中なので、`cookie` で文書を読み書きできる。
        unsafe {
            if !output.reread.is_empty() {
                remove_before_caret(cookie, context, composition.as_ref(), &output.reread)?;
            }
            let mut composition = composition;
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
                let composition = match composition {
                    Some(composition) => composition,
                    None => {
                        let range =
                            context.cast::<ITfInsertAtSelection>()?.InsertTextAtSelection(cookie, TF_IAS_QUERYONLY, &[])?;
                        let sink: ITfCompositionSink = self.to_object().to_interface();
                        context.cast::<ITfContextComposition>()?.StartComposition(cookie, &range, &sink)?
                    }
                };
                let range = composition.GetRange()?;
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
                if let Some(active) = self.active.borrow_mut().as_mut() {
                    active.composition = Some(composition);
                }
            } else if let Some(active) = self.active.borrow_mut().as_mut() {
                active.composition = None;
            }
            if anchor.is_none() {
                anchor = caret_rect(cookie, context);
            }
            self.refresh_window(anchor);
        }
        Ok(())
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
        self.apply(&context, output);
        self.schedule_timeout();
    }

    /// 打鍵をエンジンに渡すかどうか。渡さない打鍵は、TSF が入力先に通す。
    fn wants(&self, wparam: WPARAM, lparam: LPARAM) -> bool {
        let active = self.active.borrow();
        let Some(engine) = active.as_ref().and_then(|active| active.engine.as_ref()) else {
            return false;
        };
        match keys::translate(key_stroke(wparam, lparam), engine.view().mode) {
            Mapped::Modifier => false,
            Mapped::Toggle => true,
            Mapped::Engine(Key::Henkan | Key::Muhenkan) => true,
            Mapped::Engine(_) => engine.input_mode() != InputMode::Direct,
        }
    }

    fn key_down(&self, context: &ITfContext, wparam: WPARAM, lparam: LPARAM) -> bool {
        let output = {
            let mut active = self.active.borrow_mut();
            let Some(active) = active.as_mut() else {
                return false;
            };
            let now = Self::now(active);
            active.context = Some(context.clone());
            let Some(engine) = active.engine.as_mut() else {
                return false;
            };
            let key = match keys::translate(key_stroke(wparam, lparam), engine.view().mode) {
                Mapped::Modifier => return false,
                Mapped::Toggle if engine.input_mode() == InputMode::Direct => Key::Henkan,
                Mapped::Toggle => Key::Muhenkan,
                Mapped::Engine(key) => key,
            };
            if engine.input_mode() == InputMode::Direct && !matches!(key, Key::Henkan) {
                return false;
            }
            let was_direct = engine.input_mode() == InputMode::Direct;
            let output = engine.handle(key, now);
            (output, was_direct != (engine.input_mode() == InputMode::Direct))
        };
        let (output, mode_changed) = output;
        let consumed = output.consumed;
        self.apply(context, output);
        if mode_changed {
            self.sync_compartment();
        }
        self.schedule_timeout();
        consumed
    }

    /// 未確定文字列を表示のとおりに確定する。入力先が切り替わる時と、無効にされる時に呼ぶ。
    ///
    /// 入力先の文書には、表示している文字列がそのまま入っているので、書き直さずに未確定文字列を終わらせる。
    /// 編集セッションは後から動くことがあるので、終わらせる未確定文字列はここで取り出して渡す。
    fn commit_all(&self) {
        let (composition, context, client_id) = {
            let mut active = self.active.borrow_mut();
            let Some(active) = active.as_mut() else {
                return;
            };
            if let Some(engine) = active.engine.as_mut() {
                engine.commit_composition();
            }
            if let Some(window) = &active.window {
                window.hide();
                window.cancel_timer();
            }
            (active.composition.take(), active.context.clone(), active.client_id)
        };
        let (Some(composition), Some(context)) = (composition, context) else {
            return;
        };
        let session: ITfEditSession = EditSession {
            action: Box::new(move |cookie| {
                // SAFETY: 編集セッションの中で、未確定文字列の範囲の後ろにキャレットを置いてから終わらせる。
                unsafe {
                    if let Ok(range) = composition.GetRange() {
                        let _ = range.Collapse(cookie, TF_ANCHOR_END);
                    }
                    composition.EndComposition(cookie)
                }
            }),
        }
        .into();
        // SAFETY: 読み書きの編集セッションを頼む。
        let _ = unsafe { context.RequestEditSession(client_id, &session, TF_ES_ASYNCDONTCARE | TF_ES_READWRITE) };
    }

    fn advise(&self, thread_manager: &ITfThreadMgr, client_id: u32) -> windows_core::Result<(u32, u32)> {
        let key_sink: ITfKeyEventSink = self.to_object().to_interface();
        let thread_sink: ITfThreadMgrEventSink = self.to_object().to_interface();
        let compartment_sink: ITfCompartmentEventSink = self.to_object().to_interface();
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

/// 確定済みの文字列のうち、キャレットの直前の `text` を消す (Ctrl+J の読み直し)。直前の文字列が
/// `text` と違う時は、利用者がキャレットを動かしたので、何もしない。
unsafe fn remove_before_caret(
    cookie: u32,
    context: &ITfContext,
    composition: Option<&ITfComposition>,
    text: &str,
) -> windows_core::Result<()> {
    if composition.is_some() {
        return Ok(());
    }
    let expected: Vec<u16> = text.encode_utf16().collect();
    // SAFETY: 呼び出し元は編集セッションの中。
    unsafe {
        let range = context.cast::<ITfInsertAtSelection>()?.InsertTextAtSelection(cookie, TF_IAS_QUERYONLY, &[])?;
        let mut moved = 0;
        range.ShiftStart(cookie, -(expected.len() as i32), &mut moved, std::ptr::null())?;
        let mut buffer = vec![0u16; expected.len()];
        let mut length = 0;
        range.GetText(cookie, 0, &mut buffer, &mut length)?;
        if buffer[..length as usize] == expected[..] {
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
            composition: None,
            context: None,
            window: CandidateWindow::new(),
            setting_compartment: false,
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
    fn OnSetFocus(&self, _foreground: BOOL) -> windows_core::Result<()> {
        Ok(())
    }

    fn OnTestKeyDown(&self, _context: Ref<ITfContext>, wparam: WPARAM, lparam: LPARAM) -> windows_core::Result<BOOL> {
        Ok(self.wants(wparam, lparam).into())
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
        if let Some(active) = self.active.borrow_mut().as_mut() {
            active.composition = None;
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
        let (thread_manager, output) = {
            let mut active = self.active.borrow_mut();
            let Some(active) = active.as_mut() else {
                return Ok(());
            };
            if active.setting_compartment {
                return Ok(());
            }
            let thread_manager = active.thread_manager.clone();
            // SAFETY: スレッドの compartment の値を読む。
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
            let Some(engine) = active.engine.as_mut() else {
                return Ok(());
            };
            let output = match (open, engine.input_mode() == InputMode::Direct) {
                (true, true) => engine.set_input_mode(engine.kana_key_mode()),
                (false, false) => engine.set_input_mode(InputMode::Direct),
                _ => return Ok(()),
            };
            (thread_manager, output)
        };
        // SAFETY: フォーカスのある文書の、いちばん上のコンテキストに反映する。
        let context = unsafe { thread_manager.GetFocus().and_then(|document| document.GetTop()) };
        if let Ok(context) = context {
            self.apply(&context, output);
        }
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

// ITfTextInputProcessor は ITfTextInputProcessorEx の基底。QueryInterface で両方を返せるように使う。
#[allow(dead_code)]
fn _base_interface(processor: &ITfTextInputProcessorEx) -> windows_core::Result<ITfTextInputProcessor> {
    processor.cast()
}
