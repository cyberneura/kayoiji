//! TSF のテキストサービスとしての入口。DLL が書き出す関数 (`DllGetClassObject` など) と、クラスファクトリー。

mod register;
mod service;
mod window;

use std::ffi::c_void;
use std::sync::atomic::{AtomicIsize, Ordering};

use windows::Win32::Foundation::{CLASS_E_CLASSNOTAVAILABLE, CLASS_E_NOAGGREGATION, E_POINTER, HINSTANCE, S_FALSE, S_OK};
use windows::Win32::System::Com::{IClassFactory, IClassFactory_Impl};
use windows::Win32::System::SystemServices::DLL_PROCESS_ATTACH;
use windows_core::{implement, IUnknown, Interface, Ref, BOOL, GUID, HRESULT};

/// テキストサービスの COM のクラス ID。
pub const CLSID_KAYOIJI: GUID = GUID::from_u128(0xFAF85930_D466_420D_B9FF_034ECEC1BBAC);
/// 入力プロファイル (日本語の入力方式の一覧に並ぶ 1 行) の ID。
pub const PROFILE_KAYOIJI: GUID = GUID::from_u128(0xAAB26FD0_2114_4B57_8367_808FAC79AF92);
/// 日本語 (日本) の言語 ID。
pub const LANGID_JAPANESE: u16 = 0x0411;

/// この DLL のモジュールハンドル。辞書のある場所と、登録する DLL のパスを知るために使う。
static MODULE: AtomicIsize = AtomicIsize::new(0);

fn module() -> HINSTANCE {
    HINSTANCE(MODULE.load(Ordering::Relaxed) as *mut c_void)
}

#[unsafe(no_mangle)]
extern "system" fn DllMain(instance: HINSTANCE, reason: u32, _reserved: *mut c_void) -> BOOL {
    if reason == DLL_PROCESS_ATTACH {
        MODULE.store(instance.0 as isize, Ordering::Relaxed);
    }
    BOOL::from(true)
}

/// # Safety
///
/// COM の規約どおり、`clsid` と `iid` は有効な GUID を、`object` は書き込める場所を指す。
#[unsafe(no_mangle)]
unsafe extern "system" fn DllGetClassObject(clsid: *const GUID, iid: *const GUID, object: *mut *mut c_void) -> HRESULT {
    if clsid.is_null() || iid.is_null() || object.is_null() {
        return E_POINTER;
    }
    // SAFETY: 上で null でないことを確かめた。
    unsafe {
        *object = std::ptr::null_mut();
        if *clsid != CLSID_KAYOIJI {
            return CLASS_E_CLASSNOTAVAILABLE;
        }
        let factory: IClassFactory = ClassFactory.into();
        factory.query(iid, object)
    }
}

/// 入力先のアプリが動いている間に DLL を外すと、候補ウインドウやタイマーが残ることがあるので、外させない。
#[unsafe(no_mangle)]
extern "system" fn DllCanUnloadNow() -> HRESULT {
    S_FALSE
}

#[unsafe(no_mangle)]
extern "system" fn DllRegisterServer() -> HRESULT {
    match register::register() {
        Ok(()) => S_OK,
        Err(error) => {
            // 途中まで登録した分を残さない。
            let _ = register::unregister();
            error.code()
        }
    }
}

#[unsafe(no_mangle)]
extern "system" fn DllUnregisterServer() -> HRESULT {
    match register::unregister() {
        Ok(()) => S_OK,
        Err(error) => error.code(),
    }
}

#[implement(IClassFactory)]
struct ClassFactory;

impl IClassFactory_Impl for ClassFactory_Impl {
    fn CreateInstance(&self, outer: Ref<IUnknown>, iid: *const GUID, object: *mut *mut c_void) -> windows_core::Result<()> {
        if object.is_null() || iid.is_null() {
            return Err(E_POINTER.into());
        }
        // SAFETY: 上で null でないことを確かめた。
        unsafe { *object = std::ptr::null_mut() };
        if outer.is_some() {
            return Err(CLASS_E_NOAGGREGATION.into());
        }
        let service: IUnknown = service::TextService::new().into();
        // SAFETY: `iid` と `object` は COM の呼び出し元が渡した有効なポインター。
        unsafe { service.query(iid, object) }.ok()
    }

    fn LockServer(&self, _lock: BOOL) -> windows_core::Result<()> {
        Ok(())
    }
}
