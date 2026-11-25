// リリースビルドの Windows でコンソールウインドウを出さない。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    kayoiji_pad_lib::run()
}
