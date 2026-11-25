// アプリアイコンの元画像 (1024x1024 PNG) を描く。
// 使い方: swift scripts/make-icon.swift src-tauri/icons/source.png && pnpm tauri icon src-tauri/icons/source.png
import AppKit

let size = 1024.0
let path = CommandLine.arguments.count > 1 ? CommandLine.arguments[1] : "icon.png"

guard
    let bitmap = NSBitmapImageRep(
        bitmapDataPlanes: nil, pixelsWide: Int(size), pixelsHigh: Int(size), bitsPerSample: 8,
        samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB,
        bytesPerRow: 0, bitsPerPixel: 0),
    let context = NSGraphicsContext(bitmapImageRep: bitmap)
else {
    fatalError("could not create the drawing context")
}
NSGraphicsContext.current = context

// macOS のアイコンは、周囲に余白を取った角丸の四角形に描く。
let inset = 100.0
let tile = NSRect(x: inset, y: inset, width: size - inset * 2, height: size - inset * 2)
let shape = NSBezierPath(roundedRect: tile, xRadius: 185, yRadius: 185)
let gradient = NSGradient(
    starting: NSColor(calibratedRed: 0.31, green: 0.27, blue: 0.90, alpha: 1),
    ending: NSColor(calibratedRed: 0.05, green: 0.58, blue: 0.53, alpha: 1))!
gradient.draw(in: shape, angle: -55)

// 「通い路」: 2 つの言語の間を行き来する道を、交差する 2 本の弧で表す。
NSColor(calibratedWhite: 1, alpha: 0.28).setStroke()
for (startY, endY) in [(330.0, 694.0), (694.0, 330.0)] {
    let road = NSBezierPath()
    road.lineWidth = 46
    road.lineCapStyle = .round
    road.move(to: NSPoint(x: 210, y: startY))
    road.curve(
        to: NSPoint(x: 814, y: endY), controlPoint1: NSPoint(x: 470, y: startY),
        controlPoint2: NSPoint(x: 554, y: endY))
    road.stroke()
}

let glyph = NSAttributedString(
    string: "通",
    attributes: [
        .font: NSFont(name: "HiraMinProN-W6", size: 520) ?? NSFont.systemFont(ofSize: 520, weight: .bold),
        .foregroundColor: NSColor.white,
    ])
let glyphSize = glyph.size()
glyph.draw(at: NSPoint(x: (size - glyphSize.width) / 2, y: (size - glyphSize.height) / 2 + 10))

context.flushGraphics()
guard let png = bitmap.representation(using: .png, properties: [:]) else {
    fatalError("could not encode the PNG")
}
try png.write(to: URL(fileURLWithPath: path))
