// Draws the app icon: the radio's face, simplified, on a rounded square.
// Usage: swift scripts/icon.swift OUT.png      (see scripts/make-icon.sh)
import AppKit

let side: CGFloat = 1024
let space = CGColorSpaceCreateDeviceRGB()
let context = CGContext(
    data: nil, width: Int(side), height: Int(side), bitsPerComponent: 8, bytesPerRow: 0,
    space: space, bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
// Work with the origin at the top left, like the app's own layout.
context.translateBy(x: 0, y: side)
context.scaleBy(x: 1, y: -1)
NSGraphicsContext.current = NSGraphicsContext(cgContext: context, flipped: true)

func colour(_ hex: UInt32, alpha: CGFloat = 1) -> CGColor {
    CGColor(
        red: CGFloat((hex >> 16) & 0xff) / 255, green: CGFloat((hex >> 8) & 0xff) / 255,
        blue: CGFloat(hex & 0xff) / 255, alpha: alpha)
}

/// Fills a shape with a top-to-bottom gradient.
func fill(_ path: CGPath, top: UInt32, bottom: UInt32) {
    let box = path.boundingBox
    context.saveGState()
    context.addPath(path)
    context.clip()
    let gradient = CGGradient(
        colorsSpace: space, colors: [colour(top), colour(bottom)] as CFArray, locations: [0, 1])!
    context.drawLinearGradient(
        gradient, start: CGPoint(x: box.midX, y: box.minY), end: CGPoint(x: box.midX, y: box.maxY),
        options: [])
    context.restoreGState()
}

func rounded(_ rect: CGRect, _ radius: CGFloat) -> CGPath {
    CGPath(roundedRect: rect, cornerWidth: radius, cornerHeight: radius, transform: nil)
}

func circle(_ centre: CGPoint, _ radius: CGFloat) -> CGPath {
    CGPath(ellipseIn: CGRect(x: centre.x - radius, y: centre.y - radius, width: radius * 2, height: radius * 2), transform: nil)
}

let ink: UInt32 = 0x2a1a0c
let cream: UInt32 = 0xf3e6c4

// The body: dark chrome with ribs, sitting on a soft shadow.
let body = rounded(CGRect(x: 100, y: 100, width: 824, height: 824), 185)
context.saveGState()
context.setShadow(offset: CGSize(width: 0, height: -14), blur: 36, color: colour(0x000000, alpha: 0.45))
context.addPath(body)
context.setFillColor(colour(0x2b2e33))
context.fillPath()
context.restoreGState()
fill(body, top: 0x565a61, bottom: 0x2b2e33)

context.saveGState()
context.addPath(body)
context.clip()
var y: CGFloat = 126
while y < 924 {
    context.setFillColor(colour(0x000000, alpha: 0.36))
    context.fill(CGRect(x: 100, y: y, width: 824, height: 4))
    context.setFillColor(colour(0xffffff, alpha: 0.09))
    context.fill(CGRect(x: 100, y: y + 4, width: 824, height: 3))
    y += 26
}
context.restoreGState()

// The dial: a chrome bezel around dark glass.
fill(rounded(CGRect(x: 176, y: 206, width: 672, height: 338), 46), top: 0x9aa0a7, bottom: 0x33373c)
let glass = CGRect(x: 194, y: 224, width: 636, height: 302)
fill(rounded(glass, 30), top: 0x0f0d0a, bottom: 0x2a2116)

// Numerals and the scale beneath them.
let numerals = ["6", "8", "10", "12", "14"]
let font = NSFont(name: "DINCondensed-Bold", size: 104) ?? NSFont.boldSystemFont(ofSize: 92)
let attributes: [NSAttributedString.Key: Any] = [.font: font, .foregroundColor: NSColor(cgColor: colour(cream))!]
let scaleLeft = glass.minX + 62
let scaleWidth = glass.width - 124
for (index, numeral) in numerals.enumerated() {
    let text = NSAttributedString(string: numeral, attributes: attributes)
    let centre = scaleLeft + scaleWidth * CGFloat(index) / CGFloat(numerals.count - 1)
    text.draw(at: CGPoint(x: centre - text.size().width / 2, y: glass.minY + 14))
}
context.setFillColor(colour(cream))
for tick in 0...28 {
    let x = scaleLeft + scaleWidth * CGFloat(tick) / 28
    let height: CGFloat = tick % 7 == 0 ? 34 : 18
    context.fill(CGRect(x: x - 2.5, y: glass.minY + 160 - height, width: 5, height: height))
}

// The needle, and the lit station window.
let needle = CGRect(x: scaleLeft + scaleWidth * 0.63, y: glass.minY + 12, width: 13, height: 156)
context.saveGState()
context.setShadow(offset: .zero, blur: 14, color: colour(0xff4a26, alpha: 0.7))
context.addPath(rounded(needle, 4))
context.setFillColor(colour(0xff4a26))
context.fillPath()
context.restoreGState()
fill(rounded(CGRect(x: glass.minX + 20, y: glass.minY + 186, width: glass.width - 40, height: 94), 14), top: 0xffc35f, bottom: 0xee8a18)

// Two knobs with three keys between them.
for centreX in [CGFloat(272), 752] {
    let centre = CGPoint(x: centreX, y: 716)
    context.saveGState()
    context.setShadow(offset: CGSize(width: 0, height: -8), blur: 18, color: colour(0x000000, alpha: 0.5))
    context.addPath(circle(centre, 96))
    context.setFillColor(colour(0x363a3f))
    context.fillPath()
    context.restoreGState()
    fill(circle(centre, 96), top: 0x9aa0a7, bottom: 0x363a3f)
    context.addPath(circle(centre, 76))
    context.setFillColor(colour(0x6b5a3a))
    context.fillPath()
    fill(circle(centre, 73), top: 0xfaf0d6, bottom: 0xcbb688)
    context.addPath(circle(CGPoint(x: centre.x, y: centre.y - 46), 11))
    context.setFillColor(colour(ink))
    context.fillPath()
}
for key in 0..<3 {
    let rect = CGRect(x: 395 + CGFloat(key) * 80, y: 678, width: 68, height: 76)
    context.addPath(rounded(rect.insetBy(dx: -2, dy: -2), 15))
    context.setFillColor(colour(0x6b5a3a))
    context.fillPath()
    fill(rounded(rect, 13), top: 0xf8eed3, bottom: 0xd6c397)
}

let image = NSBitmapImageRep(cgImage: context.makeImage()!)
try! image.representation(using: .png, properties: [:])!.write(to: URL(fileURLWithPath: CommandLine.arguments[1]))
