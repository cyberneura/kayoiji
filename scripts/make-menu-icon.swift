// 入力メニュー (メニューバーの入力ソース) に表示するアイコンを PDF で描く。
// 使い方: swift scripts/make-menu-icon.swift <出力先の PDF> [描く文字]
//   swift scripts/make-menu-icon.swift src-tauri/icons/menu-icon.pdf 通              (自動切り替え)
//   swift scripts/make-menu-icon.swift src-tauri/icons/menu-icon-japanese.pdf あ     (日本語入力)
//   swift scripts/make-menu-icon.swift src-tauri/icons/menu-icon-roman.pdf A_        (直接入力)
import AppKit

let size = 18.0
let path = CommandLine.arguments.count > 1 ? CommandLine.arguments[1] : "menu-icon.pdf"
let character = CommandLine.arguments.count > 2 ? CommandLine.arguments[2] : "通"
var box = CGRect(x: 0, y: 0, width: size, height: size)
guard let context = CGContext(URL(fileURLWithPath: path) as CFURL, mediaBox: &box, nil) else {
    fatalError("could not create the PDF context")
}
context.beginPDFPage(nil)
NSGraphicsContext.current = NSGraphicsContext(cgContext: context, flipped: false)

// テンプレート画像として扱われるので、黒で描けばメニューバーの色に合わせて表示される。
func text(fontSize: Double) -> NSAttributedString {
    NSAttributedString(
        string: character,
        attributes: [
            .font: NSFont(name: "HiraginoSans-W6", size: fontSize)
                ?? NSFont.systemFont(ofSize: fontSize, weight: .bold),
            .foregroundColor: NSColor.black,
        ])
}
// 2 文字以上を描く時は、枠に収まる大きさまで文字を小さくする。
var fontSize = 15.0
while text(fontSize: fontSize).size().width > size && fontSize > 6 {
    fontSize -= 0.5
}
let glyph = text(fontSize: fontSize)
let glyphSize = glyph.size()
glyph.draw(at: NSPoint(x: (size - glyphSize.width) / 2, y: (size - glyphSize.height) / 2))

context.endPDFPage()
context.closePDF()
