// Renders the app icon (white ear on an indigo rounded square) into <out>/AppIcon.icns.
// Usage: swift icon.swift <Resources dir>
import AppKit

let out = URL(fileURLWithPath: CommandLine.arguments[1])
let set = FileManager.default.temporaryDirectory.appendingPathComponent("AppIcon.iconset")
try? FileManager.default.removeItem(at: set)
try FileManager.default.createDirectory(at: set, withIntermediateDirectories: true)

func render(_ px: Int) -> Data {
    let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: px, pixelsHigh: px, bitsPerSample: 8,
                               samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB,
                               bytesPerRow: 0, bitsPerPixel: 0)!
    NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
    let s = CGFloat(px), inset = s * 0.1, box = NSRect(x: inset, y: inset, width: s - 2 * inset, height: s - 2 * inset)
    let bg = NSBezierPath(roundedRect: box, xRadius: box.width * 0.225, yRadius: box.width * 0.225)
    NSGradient(starting: NSColor(red: 0.36, green: 0.33, blue: 0.95, alpha: 1),
               ending: NSColor(red: 0.18, green: 0.13, blue: 0.55, alpha: 1))!.draw(in: bg, angle: -90)
    let cfg = NSImage.SymbolConfiguration(pointSize: s * 0.5, weight: .medium)
        .applying(.init(paletteColors: [.white]))
    if let ear = NSImage(systemSymbolName: "ear", accessibilityDescription: nil)?.withSymbolConfiguration(cfg) {
        let r = NSRect(x: (s - ear.size.width) / 2, y: (s - ear.size.height) / 2, width: ear.size.width, height: ear.size.height)
        ear.draw(in: r)
    }
    NSGraphicsContext.current = nil
    return rep.representation(using: .png, properties: [:])!
}

for size in [16, 32, 128, 256, 512] {
    try render(size).write(to: set.appendingPathComponent("icon_\(size)x\(size).png"))
    try render(size * 2).write(to: set.appendingPathComponent("icon_\(size)x\(size)@2x.png"))
}
let p = Process()
p.executableURL = URL(fileURLWithPath: "/usr/bin/iconutil")
p.arguments = ["-c", "icns", set.path, "-o", out.appendingPathComponent("AppIcon.icns").path]
try p.run()
p.waitUntilExit()
exit(p.terminationStatus)
