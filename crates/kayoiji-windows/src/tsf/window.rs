//! 候補ウインドウ。入力先のプロセスの中で、Win32 のポップアップのウインドウに GDI で描く。
//! WebView を入力先のプロセスごとに起こすと重いので、macOS 版の候補ウインドウ (WebView) とは別に作る。
//!
//! ウインドウは入力先のスレッドで作り、そのスレッドのメッセージループで描く。フォーカスを奪わないように、
//! WS_EX_NOACTIVATE で出す。自動切り替えの打鍵の間隔を測るタイマーも、このウインドウで受ける。

use std::cell::RefCell;

use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateFontW, CreateSolidBrush, DeleteObject, DrawTextW, EndPaint, FillRect, GetDC, GetTextExtentPoint32W,
    InvalidateRect, ReleaseDC, SelectObject, SetBkMode, SetTextColor, CLEARTYPE_QUALITY, DEFAULT_CHARSET, DT_LEFT,
    DT_NOPREFIX, DT_SINGLELINE, DT_VCENTER, FF_DONTCARE, FW_NORMAL, HFONT, OUT_DEFAULT_PRECIS, PAINTSTRUCT, TRANSPARENT,
    CLIP_DEFAULT_PRECIS, DEFAULT_PITCH, MonitorFromPoint, GetMonitorInfoW, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, KillTimer, RegisterClassW, SetTimer, SetWindowPos, ShowWindow,
    HWND_TOPMOST, SWP_NOACTIVATE, SW_HIDE, SW_SHOWNOACTIVATE, WM_PAINT, WM_TIMER, WNDCLASSW, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP, CS_DROPSHADOW, WS_BORDER,
};
use windows_core::{w, PCWSTR};

use crate::panel::Panel;

use super::module;

const CLASS_NAME: PCWSTR = w!("KayoijiCandidateWindow");
const TIMER_ID: usize = 1;
const PADDING: i32 = 6;
const LINE_GAP: i32 = 2;
const FOOTER_GAP: i32 = 4;
/// 絵文字の格子の 1 マスの最小の幅。文字の幅に合わせると、行ごとにずれる。
const GRID_CELL: i32 = 30;

thread_local! {
    /// このスレッドの候補ウインドウに描く内容。ウインドウの手続きから読む。
    static CONTENT: RefCell<Panel> = RefCell::new(Panel::default());
    /// 打鍵の間隔のタイマーが切れた時に呼ぶ関数。テキストサービスが設定する。
    static ON_TIMER: RefCell<Option<Box<dyn Fn()>>> = const { RefCell::new(None) };
}

pub struct CandidateWindow {
    hwnd: HWND,
    font: HFONT,
    small_font: HFONT,
}

fn register_class() {
    let class = WNDCLASSW {
        style: CS_DROPSHADOW,
        lpfnWndProc: Some(window_proc),
        hInstance: module(),
        lpszClassName: CLASS_NAME,
        ..Default::default()
    };
    // SAFETY: 同じ名前のクラスを 2 度登録すると失敗するだけ (スレッドごとにウインドウを作る)。
    unsafe { RegisterClassW(&class) };
}

fn font(height: i32) -> HFONT {
    // SAFETY: 文字の高さと書体の名前だけを指定する。
    unsafe {
        CreateFontW(
            -height,
            0,
            0,
            0,
            FW_NORMAL.0 as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            CLEARTYPE_QUALITY,
            DEFAULT_PITCH.0 as u32 | FF_DONTCARE.0 as u32,
            w!("Yu Gothic UI"),
        )
    }
}

impl CandidateWindow {
    pub fn new() -> Option<Self> {
        register_class();
        // SAFETY: 登録したクラスで、表示しないポップアップのウインドウを作る。
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
                CLASS_NAME,
                PCWSTR::null(),
                WS_POPUP | WS_BORDER,
                0,
                0,
                0,
                0,
                None,
                None,
                Some(module()),
                None,
            )
        }
        .ok()?;
        Some(Self {
            hwnd,
            font: font(18),
            small_font: font(13),
        })
    }

    /// `anchor` (未確定文字列の画面上の範囲) の下に `panel` を出す。`panel` が空なら隠す。
    pub fn show(&self, panel: Panel, anchor: Option<RECT>) {
        let empty = panel.is_empty();
        let size = (!empty).then(|| self.measure(&panel));
        CONTENT.with(|content| *content.borrow_mut() = panel);
        // SAFETY: このスレッドで作ったウインドウを操作する。
        unsafe {
            let (Some((width, height)), Some(anchor)) = (size, anchor) else {
                let _ = ShowWindow(self.hwnd, SW_HIDE);
                return;
            };
            let (x, y) = position(anchor, width, height);
            let _ = SetWindowPos(self.hwnd, Some(HWND_TOPMOST), x, y, width, height, SWP_NOACTIVATE);
            let _ = InvalidateRect(Some(self.hwnd), None, true);
            let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
        }
    }

    pub fn hide(&self) {
        // SAFETY: このスレッドで作ったウインドウを隠す。
        let _ = unsafe { ShowWindow(self.hwnd, SW_HIDE) };
    }

    /// `delay_ms` の後に `on_timer` を呼ぶ。前に設定したタイマーは置き換える。
    pub fn set_timer(&self, delay_ms: u32, on_timer: Box<dyn Fn()>) {
        ON_TIMER.with(|slot| *slot.borrow_mut() = Some(on_timer));
        // SAFETY: このスレッドのウインドウにタイマーを設定する。
        unsafe { SetTimer(Some(self.hwnd), TIMER_ID, delay_ms, None) };
    }

    pub fn cancel_timer(&self) {
        ON_TIMER.with(|slot| slot.borrow_mut().take());
        // SAFETY: このスレッドのウインドウのタイマーを止める。
        let _ = unsafe { KillTimer(Some(self.hwnd), TIMER_ID) };
    }

    fn measure(&self, panel: &Panel) -> (i32, i32) {
        // SAFETY: ウインドウの DC で文字の大きさを測り、DC を返す。
        unsafe {
            let dc = GetDC(Some(self.hwnd));
            let previous = SelectObject(dc, self.font.into());
            let mut width = 0;
            let mut height = PADDING;
            for row in &panel.rows {
                let (row_width, row_height) = row_size(dc, row.iter().map(|cell| cell.text.as_str()), panel.grid);
                width = width.max(row_width);
                height += row_height + LINE_GAP;
            }
            if !panel.footer.is_empty() {
                SelectObject(dc, self.small_font.into());
                let size = text_size(dc, &panel.footer);
                width = width.max(size.cx);
                height += FOOTER_GAP + size.cy;
            }
            SelectObject(dc, previous);
            ReleaseDC(Some(self.hwnd), dc);
            (width + PADDING * 2, height + PADDING)
        }
    }
}

impl Drop for CandidateWindow {
    fn drop(&mut self) {
        self.cancel_timer();
        // SAFETY: このスレッドで作ったウインドウとフォントを片付ける。
        unsafe {
            let _ = DestroyWindow(self.hwnd);
            let _ = DeleteObject(self.font.into());
            let _ = DeleteObject(self.small_font.into());
        }
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().collect()
}

fn text_size(dc: windows::Win32::Graphics::Gdi::HDC, text: &str) -> SIZE {
    let mut size = SIZE::default();
    // SAFETY: `size` は書き込める場所を指す。
    let _ = unsafe { GetTextExtentPoint32W(dc, &wide(text), &mut size) };
    size
}

fn cell_width(dc: windows::Win32::Graphics::Gdi::HDC, text: &str, grid: bool) -> i32 {
    let width = text_size(dc, text).cx + PADDING;
    if grid {
        width.max(GRID_CELL)
    } else {
        width
    }
}

fn row_size<'a>(dc: windows::Win32::Graphics::Gdi::HDC, cells: impl Iterator<Item = &'a str>, grid: bool) -> (i32, i32) {
    cells.fold((0, 0), |(width, height), text| {
        (width + cell_width(dc, text, grid), height.max(text_size(dc, text).cy))
    })
}

/// 未確定文字列の下に置く。画面の下端からはみ出す時は上に、右端からはみ出す時は左に寄せる。
fn position(anchor: RECT, width: i32, height: i32) -> (i32, i32) {
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    let point = POINT {
        x: anchor.left,
        y: anchor.bottom,
    };
    // SAFETY: `info` は cbSize を設定した書き込める場所を指す。
    let found = unsafe { GetMonitorInfoW(MonitorFromPoint(point, MONITOR_DEFAULTTONEAREST), &mut info) }.as_bool();
    if !found {
        return (anchor.left, anchor.bottom);
    }
    let work = info.rcWork;
    let y = if anchor.bottom + height > work.bottom {
        anchor.top - height
    } else {
        anchor.bottom
    };
    let x = anchor.left.min(work.right - width).max(work.left);
    (x, y)
}

extern "system" fn window_proc(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match message {
        WM_PAINT => {
            paint(hwnd);
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == TIMER_ID => {
            // SAFETY: このウインドウのタイマーを止める。1 回だけ呼ぶ。
            let _ = unsafe { KillTimer(Some(hwnd), TIMER_ID) };
            let on_timer = ON_TIMER.with(|slot| slot.borrow_mut().take());
            if let Some(on_timer) = on_timer {
                on_timer();
            }
            LRESULT(0)
        }
        // SAFETY: ほかのメッセージは既定の手続きに任せる。
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

fn paint(hwnd: HWND) {
    let mut paint = PAINTSTRUCT::default();
    // SAFETY: WM_PAINT の中で、BeginPaint と EndPaint の間だけ DC を使う。
    unsafe {
        let dc = BeginPaint(hwnd, &mut paint);
        let background = CreateSolidBrush(COLORREF(0x00FFFFFF));
        let highlight = CreateSolidBrush(COLORREF(0x00E5464F));
        FillRect(dc, &paint.rcPaint, background);
        SetBkMode(dc, TRANSPARENT);
        let normal = font(18);
        let small = font(13);
        let previous = SelectObject(dc, normal.into());
        CONTENT.with(|content| {
            let panel = content.borrow();
            let mut y = PADDING;
            for row in &panel.rows {
                let (_, height) = row_size(dc, row.iter().map(|cell| cell.text.as_str()), panel.grid);
                let mut x = PADDING;
                for cell in row {
                    let width = cell_width(dc, &cell.text, panel.grid);
                    let mut rect = RECT {
                        left: x,
                        top: y,
                        right: x + width,
                        bottom: y + height,
                    };
                    if cell.selected {
                        FillRect(dc, &rect, highlight);
                        SetTextColor(dc, COLORREF(0x00FFFFFF));
                    } else {
                        SetTextColor(dc, COLORREF(0x00202020));
                    }
                    rect.left += PADDING / 2;
                    let mut text = wide(&cell.text);
                    DrawTextW(dc, &mut text, &mut rect, DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX);
                    x += width;
                }
                y += height + LINE_GAP;
            }
            if !panel.footer.is_empty() {
                SelectObject(dc, small.into());
                SetTextColor(dc, COLORREF(0x00707070));
                let size = text_size(dc, &panel.footer);
                let mut rect = RECT {
                    left: PADDING,
                    top: y + FOOTER_GAP,
                    right: PADDING + size.cx + PADDING,
                    bottom: y + FOOTER_GAP + size.cy,
                };
                let mut text = wide(&panel.footer);
                DrawTextW(dc, &mut text, &mut rect, DT_LEFT | DT_SINGLELINE | DT_NOPREFIX);
            }
        });
        SelectObject(dc, previous);
        let _ = DeleteObject(normal.into());
        let _ = DeleteObject(small.into());
        let _ = DeleteObject(background.into());
        let _ = DeleteObject(highlight.into());
        let _ = EndPaint(hwnd, &paint);
    }
}
