//! DLL を COM のサーバーと TSF のテキストサービスとして登録する (`regsvr32` から呼ばれる)。
//! 登録はマシン全体に効くので、管理者の権限で実行する (インストーラーが行う)。32 bit の DLL を 32 bit の
//! regsvr32 で登録すると、レジストリの書き込み先は OS が WOW6432Node に振り分ける。

use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, MAX_PATH, WIN32_ERROR};
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED};
use windows::Win32::System::LibraryLoader::GetModuleFileNameW;
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteTreeW, RegSetValueExW, HKEY, HKEY_LOCAL_MACHINE, KEY_WRITE, REG_OPTION_NON_VOLATILE,
    REG_SZ,
};
use windows::Win32::UI::Input::KeyboardAndMouse::HKL;
use windows::Win32::UI::TextServices::{
    ITfCategoryMgr, ITfInputProcessorProfileMgr, CLSID_TF_CategoryMgr, CLSID_TF_InputProcessorProfiles,
    GUID_TFCAT_TIPCAP_IMMERSIVESUPPORT, GUID_TFCAT_TIPCAP_INPUTMODECOMPARTMENT, GUID_TFCAT_TIPCAP_SYSTRAYSUPPORT,
    GUID_TFCAT_TIP_KEYBOARD,
};
use windows_core::{GUID, HSTRING, PCWSTR};

use super::{module, CLSID_KAYOIJI, LANGID_JAPANESE, PROFILE_KAYOIJI};

const DESCRIPTION: &str = "Kayoiji";

/// 入力方式の一覧に並べ、タスクバーに入力モード (あ / A) を出すための分類。
const CATEGORIES: [GUID; 4] = [
    GUID_TFCAT_TIP_KEYBOARD,
    GUID_TFCAT_TIPCAP_IMMERSIVESUPPORT,
    GUID_TFCAT_TIPCAP_SYSTRAYSUPPORT,
    GUID_TFCAT_TIPCAP_INPUTMODECOMPARTMENT,
];

fn clsid_key() -> String {
    format!("Software\\Classes\\CLSID\\{{{CLSID_KAYOIJI:?}}}")
}

fn module_path() -> windows_core::Result<Vec<u16>> {
    let mut buffer = vec![0u16; MAX_PATH as usize * 4];
    // SAFETY: `buffer` は書き込める長さを持つ。
    let length = unsafe { GetModuleFileNameW(Some(module().into()), &mut buffer) } as usize;
    if length == 0 || length >= buffer.len() {
        return Err(windows_core::Error::from_thread());
    }
    buffer.truncate(length);
    Ok(buffer)
}

fn check(status: WIN32_ERROR) -> windows_core::Result<()> {
    status.ok()
}

fn set_string(key: HKEY, name: Option<&str>, value: &[u16]) -> windows_core::Result<()> {
    let mut data: Vec<u16> = value.to_vec();
    data.push(0);
    let bytes: Vec<u8> = data.iter().flat_map(|unit| unit.to_le_bytes()).collect();
    let name = name.map(HSTRING::from);
    let name = name.as_ref().map_or(PCWSTR::null(), |name| PCWSTR(name.as_ptr()));
    // SAFETY: `key` は開いたキー、`bytes` は終端の 0 を含む UTF-16 の文字列。
    check(unsafe { RegSetValueExW(key, name, None, REG_SZ, Some(&bytes)) })
}

fn create_key(path: &str) -> windows_core::Result<HKEY> {
    let mut key = HKEY::default();
    // SAFETY: `key` は書き込める場所を指す。
    check(unsafe {
        RegCreateKeyExW(
            HKEY_LOCAL_MACHINE,
            &HSTRING::from(path),
            None,
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            None,
            &mut key,
            None,
        )
    })?;
    Ok(key)
}

/// COM のサーバーとして登録する。InprocServer32 にこの DLL のパスを書く。
fn register_server(path: &[u16]) -> windows_core::Result<()> {
    let description: Vec<u16> = DESCRIPTION.encode_utf16().collect();
    let class = create_key(&clsid_key())?;
    let result = set_string(class, None, &description);
    // SAFETY: 開いたキーを閉じる。
    let _ = unsafe { RegCloseKey(class) };
    result?;
    let server = create_key(&format!("{}\\InprocServer32", clsid_key()))?;
    let threading: Vec<u16> = "Apartment".encode_utf16().collect();
    let result = set_string(server, None, path).and_then(|()| set_string(server, Some("ThreadingModel"), &threading));
    // SAFETY: 開いたキーを閉じる。
    let _ = unsafe { RegCloseKey(server) };
    result
}

/// COM を使う間だけ初期化する。regsvr32 のスレッドは COM を初期化していない。
struct Com;

impl Com {
    fn new() -> windows_core::Result<Self> {
        // SAFETY: 対になる CoUninitialize は Drop で呼ぶ。
        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.ok()?;
        Ok(Self)
    }
}

impl Drop for Com {
    fn drop(&mut self) {
        // SAFETY: `new` で初期化した分を戻す。
        unsafe { CoUninitialize() };
    }
}

pub fn register() -> windows_core::Result<()> {
    let path = module_path()?;
    register_server(&path)?;
    let _com = Com::new()?;
    // SAFETY: TSF の COM オブジェクトを作り、文書どおりの引数で呼ぶ。
    unsafe {
        let profiles: ITfInputProcessorProfileMgr =
            CoCreateInstance(&CLSID_TF_InputProcessorProfiles, None, CLSCTX_INPROC_SERVER)?;
        let description: Vec<u16> = DESCRIPTION.encode_utf16().collect();
        profiles.RegisterProfile(
            &CLSID_KAYOIJI,
            LANGID_JAPANESE,
            &PROFILE_KAYOIJI,
            &description,
            &path,
            0,
            HKL::default(),
            0,
            true,
            0,
        )?;
        let categories: ITfCategoryMgr = CoCreateInstance(&CLSID_TF_CategoryMgr, None, CLSCTX_INPROC_SERVER)?;
        for category in &CATEGORIES {
            categories.RegisterCategory(&CLSID_KAYOIJI, category, &CLSID_KAYOIJI)?;
        }
    }
    Ok(())
}

/// 登録を外す。一部しか登録されていなくても、残りを外す。
pub fn unregister() -> windows_core::Result<()> {
    let com = Com::new();
    if com.is_ok() {
        // SAFETY: TSF の COM オブジェクトを作り、文書どおりの引数で呼ぶ。登録されていない分の失敗は無視する。
        unsafe {
            if let Ok(categories) =
                CoCreateInstance::<_, ITfCategoryMgr>(&CLSID_TF_CategoryMgr, None, CLSCTX_INPROC_SERVER)
            {
                for category in &CATEGORIES {
                    let _ = categories.UnregisterCategory(&CLSID_KAYOIJI, category, &CLSID_KAYOIJI);
                }
            }
            if let Ok(profiles) = CoCreateInstance::<_, ITfInputProcessorProfileMgr>(
                &CLSID_TF_InputProcessorProfiles,
                None,
                CLSCTX_INPROC_SERVER,
            ) {
                let _ = profiles.UnregisterProfile(&CLSID_KAYOIJI, LANGID_JAPANESE, &PROFILE_KAYOIJI, 0);
            }
        }
    }
    drop(com);
    // SAFETY: HKLM の下のキーを、その下ごと消す。
    match unsafe { RegDeleteTreeW(HKEY_LOCAL_MACHINE, &HSTRING::from(clsid_key())) } {
        status if status == ERROR_FILE_NOT_FOUND => Ok(()),
        status => check(status),
    }
}
