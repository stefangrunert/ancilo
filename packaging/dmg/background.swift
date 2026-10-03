// The background of the DMG window – drawn here, not in an image editor, so
// it can be changed and drawn again:
//
//   swift packaging/dmg/background.swift packaging/dmg
//
// writes background.png (660×400) and background@2x.png; packaging/dmg.sh
// combines them into one HiDPI TIFF. The layout matches packaging/dmg/settings.py:
// Ancilo at (170, 200), Applications at (490, 200), icons 128 pt.

import AppKit

let width: CGFloat = 660, height: CGFloat = 400
let appX: CGFloat = 170, appsX: CGFloat = 490, iconY: CGFloat = 200

func draw(scale: CGFloat, to path: String) {
    let px = NSBitmapImageRep(
        bitmapDataPlanes: nil, pixelsWide: Int(width * scale), pixelsHigh: Int(height * scale),
        bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
        colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)!
    px.size = NSSize(width: width, height: height)
    NSGraphicsContext.saveGraphicsState()
    NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: px)
    // Finder's coordinates start at the top; these at the bottom.
    let y = { (top: CGFloat) in height - top }

    // A slate between light and dark: Finder writes the icon names in black
    // (light mode) or white (dark mode) – both stay readable on it.
    NSGradient(
        starting: NSColor(srgbRed: 0x6c / 255.0, green: 0x71 / 255.0, blue: 0x8b / 255.0, alpha: 1),
        ending: NSColor(srgbRed: 0x5a / 255.0, green: 0x5f / 255.0, blue: 0x78 / 255.0, alpha: 1))!
        .draw(in: NSRect(x: 0, y: 0, width: width, height: height), angle: -90)

    // A soft light behind each icon – the names below stay on the slate.
    for x in [appX, appsX] {
        let glow = NSGradient(
            starting: NSColor(white: 1, alpha: 0.16), ending: NSColor(white: 1, alpha: 0))!
        glow.draw(fromCenter: NSPoint(x: x, y: y(iconY) + 6), radius: 0,
                  toCenter: NSPoint(x: x, y: y(iconY) + 6), radius: 86, options: [])
    }

    // The arrow: from Ancilo to Applications.
    let arrow = NSBezierPath()
    let from = NSPoint(x: appX + 100, y: y(iconY)), to = NSPoint(x: appsX - 104, y: y(iconY))
    arrow.move(to: from)
    arrow.curve(to: to, controlPoint1: NSPoint(x: from.x + 40, y: from.y + 26),
                controlPoint2: NSPoint(x: to.x - 40, y: to.y + 26))
    arrow.lineWidth = 3
    arrow.lineCapStyle = .round
    let light = NSColor(srgbRed: 0xc9 / 255.0, green: 0xd1 / 255.0, blue: 1, alpha: 1)
    light.setStroke()
    arrow.stroke()
    let head = NSBezierPath()
    head.move(to: NSPoint(x: to.x - 13, y: to.y + 10))
    head.line(to: to)
    head.line(to: NSPoint(x: to.x - 15, y: to.y - 5))
    head.lineWidth = 3
    head.lineCapStyle = .round
    head.lineJoinStyle = .round
    head.stroke()

    // What to do – in the two languages Ancilo speaks.
    func text(_ s: String, size: CGFloat, weight: NSFont.Weight, color: NSColor, top: CGFloat) {
        let style = NSMutableParagraphStyle()
        style.alignment = .center
        let attrs: [NSAttributedString.Key: Any] = [
            .font: NSFont.systemFont(ofSize: size, weight: weight),
            .foregroundColor: color, .paragraphStyle: style,
        ]
        NSAttributedString(string: s, attributes: attrs)
            .draw(in: NSRect(x: 0, y: y(top) - size * 1.4, width: width, height: size * 1.6))
    }
    text("Drag Ancilo to Applications", size: 17, weight: .semibold,
         color: NSColor(white: 1, alpha: 1), top: 40)
    text("Ziehe Ancilo in den Ordner „Programme“", size: 13, weight: .regular,
         color: NSColor(white: 1, alpha: 0.78), top: 66)
    text("Your own AI, on this Mac.", size: 12, weight: .regular,
         color: NSColor(white: 1, alpha: 0.62), top: 352)

    NSGraphicsContext.restoreGraphicsState()
    try! px.representation(using: .png, properties: [:])!.write(to: URL(fileURLWithPath: path))
}

let dir = CommandLine.arguments.count > 1 ? CommandLine.arguments[1] : "."
draw(scale: 1, to: "\(dir)/background.png")
draw(scale: 2, to: "\(dir)/background@2x.png")
