//! OS の入力ソースの状態。

/// OS の入力ソースが、文字を変換する入力メソッドになっているかどうか。
/// 英字のキーボード配列なら false。macOS ではメインスレッドから呼ぶこと。
#[cfg(target_os = "macos")]
pub fn input_method_active() -> bool {
    use std::ffi::c_void;

    #[link(name = "Carbon", kind = "framework")]
    extern "C" {
        static kTISPropertyInputSourceType: *const c_void;
        static kTISTypeKeyboardLayout: *const c_void;
        fn TISCopyCurrentKeyboardInputSource() -> *const c_void;
        fn TISGetInputSourceProperty(source: *const c_void, key: *const c_void) -> *const c_void;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFEqual(a: *const c_void, b: *const c_void) -> u8;
        fn CFRelease(object: *const c_void);
    }

    // SAFETY: 取得した入力ソースは、種類を調べた後に解放する。種類を表す値は入力ソースが持つので解放しない。
    unsafe {
        let source = TISCopyCurrentKeyboardInputSource();
        if source.is_null() {
            return false;
        }
        let source_type = TISGetInputSourceProperty(source, kTISPropertyInputSourceType);
        let is_layout = !source_type.is_null() && CFEqual(source_type, kTISTypeKeyboardLayout) != 0;
        CFRelease(source);
        !is_layout
    }
}

#[cfg(not(target_os = "macos"))]
pub fn input_method_active() -> bool {
    false
}
