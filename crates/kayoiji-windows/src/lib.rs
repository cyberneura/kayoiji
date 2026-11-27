//! Kayoiji の Windows 版。TSF (Text Services Framework) のテキストサービスとして、入力先のアプリの
//! プロセスに読み込まれる COM の DLL。キーの読み替えなど OS に依存しない部分は、どの OS でもテストできる。

pub mod keys;
pub mod panel;

#[cfg(windows)]
pub mod tsf;
