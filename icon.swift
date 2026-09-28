// Renders the app icon (white ear on an indigo rounded square) into <out>/AppIcon.icns, or with `installer` the
// release DMG's volume icon into <out>/Installer.icns: the same tile standing on a disk drive with a download badge,
// so the mounted installer doesn't look like the app itself.
// Usage: swift icon.swift <out dir> [installer]
import AppKit

let out = URL(fileURLWithPath: CommandLine.arguments[1])
let installer = CommandLine.arguments.dropFirst(2).first == "installer"
let name = installer ? "Installer" : "AppIcon"
let set = FileManager.default.temporaryDirectory.appendingPathComponent("\(name).iconset")
try? FileManager.default.removeItem(at: set)
try FileManager.default.createDirectory(at: set, withIntermediateDirectories: true)

let indigo = NSColor(red: 0.36, green: 0.33, blue: 0.95, alpha: 1), deep = NSColor(red: 0.18, green: 0.13, blue: 0.55, alpha: 1)

func tile(_ box: NSRect) {
    let bg = NSBezierPath(roundedRect: box, xRadius: box.width * 0.225, yRadius: box.width * 0.225)
    NSGradient(starting: indigo, ending: deep)!.draw(in: bg, angle: -90)
    let cfg = NSImage.SymbolConfiguration(pointSize: box.width * 0.625, weight: .medium)
        .applying(.init(paletteColors: [.white]))
    if let ear = NSImage(systemSymbolName: "ear", accessibilityDescription: nil)?.withSymbolConfiguration(cfg) {
        ear.draw(in: NSRect(x: box.midX - ear.size.width / 2, y: box.midY - ear.size.height / 2,
                            width: ear.size.width, height: ear.size.height))
    }
}

/// A silver external drive across the bottom, with a status light.
func drive(_ s: CGFloat) {
    let body = NSRect(x: s * 0.08, y: s * 0.12, width: s * 0.84, height: s * 0.3)
    NSGraphicsContext.saveGraphicsState()
    let shadow = NSShadow()
    shadow.shadowColor = NSColor.black.withAlphaComponent(0.3)
    shadow.shadowBlurRadius = s * 0.03
    shadow.shadowOffset = NSSize(width: 0, height: -s * 0.015)
    shadow.set()
    let path = NSBezierPath(roundedRect: body, xRadius: s * 0.06, yRadius: s * 0.06)
    NSColor(white: 0.8, alpha: 1).setFill()
    path.fill()
    NSGraphicsContext.restoreGraphicsState()
    NSGradient(starting: NSColor(white: 0.97, alpha: 1), ending: NSColor(white: 0.76, alpha: 1))!.draw(in: path, angle: -90)
    NSColor(white: 0.62, alpha: 1).setStroke()
    path.lineWidth = max(1, s * 0.006)
    path.stroke()
    indigo.setFill()
    NSBezierPath(ovalIn: NSRect(x: s * 0.15, y: s * 0.19, width: s * 0.04, height: s * 0.04)).fill()
}

/// White circle with an indigo down arrow: "this installs something".
func badge(_ r: NSRect) {
    NSGraphicsContext.saveGraphicsState()
    let shadow = NSShadow()
    shadow.shadowColor = deep.withAlphaComponent(0.35)
    shadow.shadowBlurRadius = r.width * 0.12
    shadow.shadowOffset = NSSize(width: 0, height: -r.width * 0.04)
    shadow.set()
    NSColor.white.setFill()
    NSBezierPath(ovalIn: r).fill()
    NSGraphicsContext.restoreGraphicsState()
    let cfg = NSImage.SymbolConfiguration(pointSize: r.width * 0.55, weight: .bold).applying(.init(paletteColors: [indigo]))
    if let arrow = NSImage(systemSymbolName: "arrow.down", accessibilityDescription: nil)?.withSymbolConfiguration(cfg) {
        arrow.draw(in: NSRect(x: r.midX - arrow.size.width / 2, y: r.midY - arrow.size.height / 2,
                              width: arrow.size.width, height: arrow.size.height))
    }
}

func render(_ px: Int) -> Data {
    let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: px, pixelsHigh: px, bitsPerSample: 8,
                               samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB,
                               bytesPerRow: 0, bitsPerPixel: 0)!
    NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
    let s = CGFloat(px)
    if installer {
        drive(s)
        tile(NSRect(x: s * 0.24, y: s * 0.3, width: s * 0.52, height: s * 0.52))
        badge(NSRect(x: s * 0.6, y: s * 0.27, width: s * 0.26, height: s * 0.26))
    } else {
        tile(NSRect(x: s * 0.1, y: s * 0.1, width: s * 0.8, height: s * 0.8))
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
p.arguments = ["-c", "icns", set.path, "-o", out.appendingPathComponent("\(name).icns").path]
try p.run()
p.waitUntilExit()
exit(p.terminationStatus)
