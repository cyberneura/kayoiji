//! OS の入力ソースの操作 (Text Input Sources)。メインスレッドで呼ぶ。
//!
//! 入力ソースの一覧に並ぶのも、メニューバーのアイコンが決まるのも、入力モードの単位。一覧に Kayoiji を
//! 1 つだけ並べたまま、アイコンを切り替えるために、次のようにする。
//!
//! - 直接入力にする時は、Kayoiji の中のモードではなく、OS の英字のキーボード配列 (ABC など) を選ぶ。
//!   アイコンは OS の英字のものになる。「かな」キーで OS が Kayoiji を選び直す
//! - 日本語入力と自動切り替えは、アイコンの違う入力モードを 2 つ持ち、設定に合う側だけを有効にする

use std::ffi::c_void;

use objc2_foundation::NSString;

type Ref = *const c_void;

#[link(name = "Carbon", kind = "framework")]
extern "C" {
    static kTISPropertyInputSourceID: Ref;
    static kTISPropertyInputSourceIsEnabled: Ref;
    static kTISPropertyInputSourceIsSelected: Ref;
    fn TISCreateInputSourceList(properties: Ref, include_all_installed: u8) -> Ref;
    fn TISGetInputSourceProperty(source: Ref, key: Ref) -> Ref;
    fn TISEnableInputSource(source: Ref) -> i32;
    fn TISDisableInputSource(source: Ref) -> i32;
    fn TISSelectInputSource(source: Ref) -> i32;
    fn TISCopyCurrentASCIICapableKeyboardLayoutInputSource() -> Ref;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFRelease(value: Ref);
    fn CFArrayGetCount(array: Ref) -> isize;
    fn CFArrayGetValueAtIndex(array: Ref, index: isize) -> Ref;
    fn CFBooleanGetValue(value: Ref) -> u8;
}

/// OS の英字のキーボード配列を選ぶ。この入力メソッドは選ばれていない状態になる。
pub fn select_ascii_layout() {
    // SAFETY: 返る入力ソースは呼び出し側が所有するので、使った後に解放する。
    unsafe {
        let source = TISCopyCurrentASCIICapableKeyboardLayoutInputSource();
        if source.is_null() {
            return;
        }
        let status = TISSelectInputSource(source);
        CFRelease(source);
        if status != 0 {
            eprintln!("kayoiji: could not select the ASCII keyboard layout: {status}");
        }
    }
}

/// 入力モード `wanted` を有効にして、`unwanted` を無効にする。`unwanted` が選ばれていれば、`wanted` を選び直す。
///
/// 利用者が Kayoiji を入力ソースに足していない時 (どちらも無効) は、何もしない。
pub fn keep_one_mode_enabled(wanted: &str, unwanted: &str) {
    // SAFETY: 一覧は呼び出し側が所有するので、最後に解放する。一覧の中の入力ソースは、一覧が生きている間だけ使う。
    unsafe {
        // 第 2 引数が真なら、有効にしていない入力ソースも含まれる。
        let sources = TISCreateInputSourceList(std::ptr::null(), 1);
        if sources.is_null() {
            return;
        }
        let find = |identifier: &str| {
            (0..CFArrayGetCount(sources))
                .map(|index| CFArrayGetValueAtIndex(sources, index))
                .find(|&source| {
                    let value = TISGetInputSourceProperty(source, kTISPropertyInputSourceID);
                    // CFString は NSString としてそのまま読める。
                    !value.is_null() && (*value.cast::<NSString>()).to_string() == identifier
                })
        };
        let flag = |source: Ref, key: Ref| {
            let value = TISGetInputSourceProperty(source, key);
            !value.is_null() && CFBooleanGetValue(value) != 0
        };
        let enabled = |source: Ref| flag(source, kTISPropertyInputSourceIsEnabled);
        let wanted_source = find(wanted);
        let unwanted_source = find(unwanted);
        if let Some(source) = wanted_source {
            let added = enabled(source) || unwanted_source.is_some_and(enabled);
            if added {
                let mut status = if enabled(source) { 0 } else { TISEnableInputSource(source) };
                if let Some(other) = unwanted_source {
                    if status == 0 && flag(other, kTISPropertyInputSourceIsSelected) {
                        status = TISSelectInputSource(source);
                    }
                    if status == 0 && enabled(other) {
                        status = TISDisableInputSource(other);
                    }
                }
                if status != 0 {
                    eprintln!("kayoiji: could not switch the enabled input mode: {status}");
                }
            }
        }
        CFRelease(sources);
    }
}
