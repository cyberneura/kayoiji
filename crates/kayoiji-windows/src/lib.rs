//! Kayoiji の Windows 版。TSF (Text Services Framework) のテキストサービスとして、入力先のアプリの
//! プロセスに読み込まれる COM の DLL。キーの読み替えなど OS に依存しない部分は、どの OS でもテストできる。

pub mod keys;
pub mod panel;

#[cfg(windows)]
pub mod tsf;

#[cfg(test)]
mod tests {
    /// 配布物に入るクレートの `Cargo.toml` (Windows 版と、そのワークスペース内の依存)。
    const SHIPPED_MANIFESTS: [&str; 2] = ["Cargo.toml", "../kayoiji-core/Cargo.toml"];

    #[test]
    fn third_party_notices_list_every_direct_dependency() {
        // Arrange
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let notices = std::fs::read_to_string(root.join("../../THIRD-PARTY-NOTICES-windows.txt")).expect("notices");
        let workspace: toml::Table =
            toml::from_str(&std::fs::read_to_string(root.join("../../Cargo.toml")).expect("manifest")).expect("toml");
        let members: Vec<&str> = workspace["workspace"]["dependencies"]
            .as_table()
            .expect("workspace dependencies")
            .iter()
            .filter(|(_, spec)| spec.get("path").is_some())
            .map(|(name, _)| name.as_str())
            .collect();

        // Act
        let mut missing = Vec::new();
        for manifest in SHIPPED_MANIFESTS {
            let manifest: toml::Table =
                toml::from_str(&std::fs::read_to_string(root.join(manifest)).expect("manifest")).expect("toml");
            let tables = std::iter::once(manifest.get("dependencies")).chain(
                manifest
                    .get("target")
                    .and_then(|targets| targets.as_table())
                    .into_iter()
                    .flat_map(|targets| targets.values().map(|target| target.get("dependencies"))),
            );
            for name in tables.flatten().filter_map(|table| table.as_table()).flat_map(|table| table.keys()) {
                if !members.contains(&name.as_str()) && !notices.contains(&format!("\n  {name} ")) {
                    missing.push(name.clone());
                }
            }
        }

        // Assert: 足りなければ `pnpm notices` で作り直す。
        assert!(missing.is_empty(), "not in THIRD-PARTY-NOTICES-windows.txt: {missing:?}");
    }
}
