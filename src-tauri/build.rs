use std::time::{SystemTime, UNIX_EPOCH};

fn main() {
    let built_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("the clock is after 1970")
        .as_secs();
    println!("cargo:rustc-env=KAYOIJI_BUILT_AT={built_at}");
    // cargo は、監視の対象が変わらなければこのスクリプトを実行し直さない。そのままでは、ソースだけを
    // 変えたビルドに古い日時が残る。リリースビルドでは存在しないファイルを対象に足して、毎回実行させる。
    // 開発ビルドでは、毎回の再コンパイルを避けるために足さない (日時は古いことがある)。
    if std::env::var("PROFILE").is_ok_and(|profile| profile == "release") {
        println!("cargo:rerun-if-changed=rerun-on-every-release-build");
    }
    tauri_build::build()
}
