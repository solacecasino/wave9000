// Renders assets/icon.png (1024x1024) and assets/icon-256.png: the
// wave9000 mark, five rounded bars like a waveform with the tallest in
// wave blue, on the same graphite body as splice9000's icon.
// Run: swift scripts/make_icon.swift
import AppKit

func render(_ size: Int) -> NSBitmapImageRep {
    let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: size, pixelsHigh: size,
                               bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
                               colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)!
    NSGraphicsContext.saveGraphicsState()
    NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
    let ctx = NSGraphicsContext.current!.cgContext
    let k = CGFloat(size) / 1024
    ctx.scaleBy(x: k, y: k)
    func color(_ hex: UInt32, _ a: CGFloat = 1) -> CGColor {
        NSColor(srgbRed: CGFloat(hex >> 16 & 0xff) / 255, green: CGFloat(hex >> 8 & 0xff) / 255,
                blue: CGFloat(hex & 0xff) / 255, alpha: a).cgColor
    }

    // Body on Apple's icon grid: 824 square, 100 inset.
    let body = CGPath(roundedRect: CGRect(x: 100, y: 100, width: 824, height: 824), cornerWidth: 185, cornerHeight: 185, transform: nil)
    ctx.saveGState()
    ctx.setShadow(offset: CGSize(width: 0, height: -12), blur: 28, color: color(0, 0.35))
    ctx.addPath(body)
    ctx.setFillColor(color(0x17181b))
    ctx.fillPath()
    ctx.restoreGState()
    ctx.saveGState()
    ctx.addPath(body)
    ctx.clip()
    let grad = CGGradient(colorsSpace: CGColorSpaceCreateDeviceRGB(), colors: [color(0x22242a), color(0x121316)] as CFArray, locations: [0, 1])!
    ctx.drawLinearGradient(grad, start: CGPoint(x: 0, y: 924), end: CGPoint(x: 0, y: 100), options: [])
    // A hairline through the middle, like a quiet track.
    ctx.setFillColor(color(0x2e3036))
    ctx.fill(CGRect(x: 100, y: 511, width: 824, height: 3))
    ctx.restoreGState()

    // Five bars, centered on the hairline.
    let heights: [CGFloat] = [0.32, 0.62, 1.0, 0.7, 0.4]
    let (left, step, width, tallest): (CGFloat, CGFloat, CGFloat, CGFloat) = (232, 120, 80, 520)
    for (k, h) in heights.enumerated() {
        let height = tallest * h
        let bar = CGRect(x: left + CGFloat(k) * step, y: 512 - height / 2, width: width, height: height)
        ctx.addPath(CGPath(roundedRect: bar, cornerWidth: width / 2, cornerHeight: width / 2, transform: nil))
        ctx.setFillColor(k == 2 ? color(0x3fb8ff) : color(0xe8e9eb))
        ctx.fillPath()
    }
    NSGraphicsContext.restoreGraphicsState()
    return rep
}

let dir = URL(fileURLWithPath: CommandLine.arguments.count > 1 ? CommandLine.arguments[1] : "assets")
try! render(1024).representation(using: .png, properties: [:])!.write(to: dir.appendingPathComponent("icon.png"))
try! render(256).representation(using: .png, properties: [:])!.write(to: dir.appendingPathComponent("icon-256.png"))
