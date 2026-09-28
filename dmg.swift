// Renders the DMG window background (title, arrow from Ozen.app to Applications, first-open hint) at 1x and 2x.
// The icon spots match dmg_settings.py. Usage: swift dmg.swift <out dir>  (writes bg.png, bg@2x.png)
import AppKit

let out = URL(fileURLWithPath: CommandLine.arguments[1])
let w: CGFloat = 640, h: CGFloat = 400  // points; the window size in dmg_settings.py
let indigo = NSColor(red: 0.36, green: 0.33, blue: 0.95, alpha: 1), deep = NSColor(red: 0.18, green: 0.13, blue: 0.55, alpha: 1)

func text(_ s: String, size: CGFloat, weight: NSFont.Weight, color: NSColor, y: CGFloat) {
    let p = NSMutableParagraphStyle()
    p.alignment = .center
    let a: [NSAttributedString.Key: Any] = [.font: NSFont.systemFont(ofSize: size, weight: weight), .foregroundColor: color,
                                            .paragraphStyle: p, .kern: size > 30 ? -0.5 : 0]
    NSAttributedString(string: s, attributes: a).draw(in: NSRect(x: 0, y: y, width: w, height: size * 1.4))
}

func render(_ scale: CGFloat) -> Data {
    let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: Int(w * scale), pixelsHigh: Int(h * scale),
                               bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
                               colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)!
    rep.size = NSSize(width: w, height: h)  // draw in points; the rep holds scale x pixels
    NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
    let all = NSRect(x: 0, y: 0, width: w, height: h)
    NSGradient(starting: NSColor(red: 0.97, green: 0.97, blue: 1, alpha: 1),
               ending: NSColor(red: 0.90, green: 0.89, blue: 0.99, alpha: 1))!.draw(in: all, angle: -90)
    // frosted cards behind the two icons and their labels
    for x in [170.0, 470.0] {
        NSGraphicsContext.saveGraphicsState()
        let shadow = NSShadow()
        shadow.shadowColor = deep.withAlphaComponent(0.12)
        shadow.shadowBlurRadius = 18
        shadow.shadowOffset = NSSize(width: 0, height: -4)
        shadow.set()
        NSColor.white.withAlphaComponent(0.7).setFill()
        NSBezierPath(roundedRect: NSRect(x: x - 85, y: 92, width: 170, height: 184), xRadius: 26, yRadius: 26).fill()
        NSGraphicsContext.restoreGraphicsState()
    }
    text("Ozen", size: 34, weight: .bold, color: deep, y: h - 78)
    text("Drag Ozen into Applications to install", size: 14, weight: .regular, color: deep.withAlphaComponent(0.65), y: h - 104)
    // dashed arrow between the icons (Finder's y = 210 from the top is 190 from the bottom)
    let line = NSBezierPath()
    line.move(to: NSPoint(x: 250, y: 190))
    line.line(to: NSPoint(x: 380, y: 190))
    line.lineWidth = 3
    line.lineCapStyle = .round
    line.setLineDash([2, 9], count: 2, phase: 0)
    indigo.setStroke()
    line.stroke()
    let head = NSBezierPath()
    head.move(to: NSPoint(x: 378, y: 202))
    head.line(to: NSPoint(x: 392, y: 190))
    head.line(to: NSPoint(x: 378, y: 178))
    head.lineWidth = 3
    head.lineCapStyle = .round
    head.lineJoinStyle = .round
    head.stroke()
    text("First open: System Settings › Privacy & Security › Open Anyway", size: 11, weight: .medium,
         color: deep.withAlphaComponent(0.5), y: 46)  // above the status bar Finder may show
    NSGraphicsContext.current = nil
    return rep.representation(using: .png, properties: [:])!
}

try render(1).write(to: out.appendingPathComponent("bg.png"))
try render(2).write(to: out.appendingPathComponent("bg@2x.png"))
