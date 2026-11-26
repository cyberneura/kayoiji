// 入力メソッドのバンドルを OS に登録し、入れ直した後も同じように使える状態にする。
// 使い方:
//   input-source state <bundle>      入力ソースとして有効にしてあれば enabled と出力する
//   input-source register <bundle>   ログインし直さなくても入力ソースの一覧に出るように登録する
//   input-source enable <bundle>     バンドルの入力モードがどれも有効でなければ、先頭の 1 つを有効にして、結果を確かめる。
//                                    OS が入力モードをまだ認識していない時は、終了コード 2 で終わる
import Carbon
import Foundation

func fail(_ message: String, code: Int32 = 1) -> Never {
    FileHandle.standardError.write(Data((message + "\n").utf8))
    exit(code)
}

func flag(_ source: TISInputSource, _ key: CFString) -> Bool {
    guard let value = TISGetInputSourceProperty(source, key) else { return false }
    return CFBooleanGetValue(Unmanaged<CFBoolean>.fromOpaque(value).takeUnretainedValue())
}

func identifier(_ source: TISInputSource) -> String {
    guard let value = TISGetInputSourceProperty(source, kTISPropertyInputSourceID) else { return "" }
    return Unmanaged<CFString>.fromOpaque(value).takeUnretainedValue() as String
}

func installedSources(bundleID: String) -> [TISInputSource] {
    let filter = [kTISPropertyBundleID as String: bundleID] as CFDictionary
    // 第 2 引数が true なら、有効にしていない入力ソースも含まれる。
    return TISCreateInputSourceList(filter, true).takeRetainedValue() as! [TISInputSource]
}

/// Info.plist に書いてある入力モードの ID。
func declaredModes(_ bundle: Bundle) -> [String] {
    let modes = bundle.infoDictionary?["ComponentInputModeDict"] as? [String: Any]
    return modes?["tsVisibleInputModeOrderedArrayKey"] as? [String] ?? []
}

let arguments = CommandLine.arguments
guard arguments.count == 3 else {
    fail("usage: input-source <state|register|enable> <path to the input method bundle>")
}
let command = arguments[1]
let path = arguments[2]

switch command {
case "state":
    // まだインストールしていない時は、何も出力しない。
    guard let bundleID = Bundle(path: path)?.bundleIdentifier else { break }
    if installedSources(bundleID: bundleID).contains(where: { flag($0, kTISPropertyInputSourceIsEnabled) }) {
        print("enabled")
    }

case "register":
    let status = TISRegisterInputSource(URL(fileURLWithPath: path) as CFURL)
    guard status == noErr else { fail("TISRegisterInputSource failed: \(status)") }

case "enable":
    guard let bundle = Bundle(path: path), let bundleID = bundle.bundleIdentifier else {
        fail("not an application bundle: \(path)")
    }
    let sources = installedSources(bundleID: bundleID)
    // 入力ソースの一覧に並ぶのは入力モードごとなので、有効にするのは 1 つだけにする。日本語入力と
    // 自動切り替えのどちらを有効にするかは設定で決まり、起動した入力メソッドが入れ替える。
    // ここでは、入れ替えた後にどれも有効でなくなった時に、先頭の入力モードを有効にする。
    let modes = declaredModes(bundle)
    let isMode = { (source: TISInputSource) in modes.contains(identifier(source)) }
    if !sources.contains(where: { isMode($0) && flag($0, kTISPropertyInputSourceIsEnabled) }),
        let first = sources.first(where: { identifier($0) == modes.first })
    {
        let status = TISEnableInputSource(first)
        guard status == noErr else { fail("TISEnableInputSource failed for \(identifier(first)): \(status)") }
    }
    // 有効にした結果は、一覧を取り直して確かめる。
    var enabled: [String: Bool] = [:]
    for source in installedSources(bundleID: bundleID) {
        enabled[identifier(source)] = flag(source, kTISPropertyInputSourceIsEnabled)
        print("  \(identifier(source)): \(enabled[identifier(source)] == true ? "enabled" : "disabled")")
    }
    let missing = modes.filter { enabled[$0] == nil }
    guard missing.isEmpty else {
        fail("OS が古い入力ソースの情報を持ったままで、次の入力モードが登録されていない: "
            + missing.joined(separator: ", ") + "\nログインし直してから、もう一度インストールする。", code: 2)
    }
    guard modes.contains(where: { enabled[$0] == true }) else {
        fail("入力モードを有効にできなかった。\n「システム設定 → キーボード → 入力ソース」で Kayoiji を追加し直す。")
    }

default:
    fail("unknown command: \(command)")
}
