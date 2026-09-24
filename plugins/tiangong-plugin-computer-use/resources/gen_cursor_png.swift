// 一次性生成脚本：按 resources/virtual-cursor.svg 同规格渲染 4x PNG。
// 用法：swift gen_cursor_png.swift <输出路径>
import CoreGraphics
import CoreText
import Foundation
import ImageIO
import UniformTypeIdentifiers

let SCALE = 4.0
let W = 128.0, H = 96.0
let PW = Int(W * SCALE), PH = Int(H * SCALE)

guard CommandLine.arguments.count > 1 else { fatalError("缺少输出路径") }
let outURL = URL(fileURLWithPath: CommandLine.arguments[1])

let colorSpace = CGColorSpace(name: CGColorSpace.sRGB)!
let ctx = CGContext(data: nil, width: PW, height: PH, bitsPerComponent: 8, bytesPerRow: 0,
                    space: colorSpace, bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
ctx.scaleBy(x: SCALE, y: SCALE)

// SVG y 向下 → CG y 向上：y' = 96 - y
func path() -> CGPath {
    let p = CGMutablePath()
    // M16 12 L16 76 L31 62 L42 88 L52 84 L41 59 L64 56 Z
    p.move(to: CGPoint(x: 16, y: 84))
    p.addLine(to: CGPoint(x: 16, y: 20))
    p.addLine(to: CGPoint(x: 31, y: 34))
    p.addLine(to: CGPoint(x: 42, y: 8))
    p.addLine(to: CGPoint(x: 52, y: 12))
    p.addLine(to: CGPoint(x: 41, y: 37))
    p.addLine(to: CGPoint(x: 64, y: 40))
    p.closeSubpath()
    return p
}

// 箭头：阴影 + 白填充 + 渐变描边
ctx.saveGState()
ctx.setShadow(offset: CGSize(width: 1.5, height: -2.5), blur: 2.5,
              color: CGColor(srgbRed: 0x31/255.0, green: 0x2e/255.0, blue: 0x81/255.0, alpha: 0.45))
let arrow = path()
ctx.addPath(arrow)
ctx.setFillColor(CGColor(srgbRed: 1, green: 1, blue: 1, alpha: 1))
ctx.setLineWidth(4)
ctx.setLineJoin(.round)
ctx.drawPath(using: .fillStroke) // 先白描边占位（同形状，保证阴影完整）
ctx.restoreGState()

// 渐变描边：clip 到 stroke 区域后填充线性渐变
ctx.saveGState()
ctx.addPath(arrow)
ctx.replacePathWithStrokedPath()
ctx.clip()
let colors = [
    CGColor(srgbRed: 0x63/255.0, green: 0x66/255.0, blue: 0xf1/255.0, alpha: 1),
    CGColor(srgbRed: 0xa8/255.0, green: 0x55/255.0, blue: 0xf7/255.0, alpha: 1),
] as CFArray
let grad = CGGradient(colorsSpace: colorSpace, colors: colors, locations: [0, 1])!
ctx.drawLinearGradient(grad, start: CGPoint(x: 16, y: 84), end: CGPoint(x: 80, y: 8), options: [])
ctx.restoreGState()

// 徽标胶囊：SVG rect(46,60,60,26) → CG y' = 96-60-26 = 10
ctx.saveGState()
ctx.setShadow(offset: CGSize(width: 1.5, height: -2.5), blur: 2.5,
              color: CGColor(srgbRed: 0x31/255.0, green: 0x2e/255.0, blue: 0x81/255.0, alpha: 0.45))
let capsule = CGPath(roundedRect: CGRect(x: 46, y: 10, width: 60, height: 26),
                     cornerWidth: 13, cornerHeight: 13, transform: nil)
ctx.addPath(capsule)
ctx.setFillColor(CGColor(srgbRed: 0x1e/255.0, green: 0x1b/255.0, blue: 0x39/255.0, alpha: 0.88))
ctx.fillPath(using: .winding)
ctx.restoreGState()
ctx.addPath(capsule)
ctx.setStrokeColor(CGColor(srgbRed: 0x8b/255.0, green: 0x5c/255.0, blue: 0xf6/255.0, alpha: 0.5))
ctx.setLineWidth(1.5)
ctx.strokePath()

// 文字「天工」：中心 SVG (76,74) → CG (76,22)，PingFang SC Semibold 16px，
// 字距 2，白色。
let font = CTFontCreateWithName("PingFangSC-Semibold" as CFString, 16, nil)
let attrs: [CFString: Any] = [
    kCTFontAttributeName: font,
    kCTForegroundColorAttributeName: CGColor(srgbRed: 1, green: 1, blue: 1, alpha: 1),
    kCTKernAttributeName: 2.0,
]
let str = CFAttributedStringCreate(nil, "天工" as CFString, attrs as CFDictionary)!
let line = CTLineCreateWithAttributedString(str)
let lineBounds = CTLineGetBoundsWithOptions(line, .useOpticalBounds)
let centerX = 76.0, centerY = 22.0
ctx.textPosition = CGPoint(x: centerX - lineBounds.width / 2, y: centerY - lineBounds.midY)
CTLineDraw(line, ctx)

// 写 PNG
let image = ctx.makeImage()!
let dest = CGImageDestinationCreateWithURL(outURL as CFURL, UTType.png.identifier as CFString, 1, nil)!
CGImageDestinationAddImage(dest, image, nil)
guard CGImageDestinationFinalize(dest) else { fatalError("PNG 写入失败") }
print("written: \(outURL.path) \(PW)x\(PH)")
