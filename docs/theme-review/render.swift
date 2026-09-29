import AppKit
for path in CommandLine.arguments.dropFirst() {
    guard let image = NSImage(contentsOfFile: path),
          let bitmap = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: 1400, pixelsHigh: 500, bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0),
          let context = NSGraphicsContext(bitmapImageRep: bitmap) else { fatalError(path) }
    NSGraphicsContext.saveGraphicsState()
    NSGraphicsContext.current = context
    image.draw(in: NSRect(x: 0, y: 0, width: 1400, height: 500))
    context.flushGraphics()
    NSGraphicsContext.restoreGraphicsState()
    let target = URL(fileURLWithPath: path).deletingPathExtension().appendingPathExtension("png")
    try bitmap.representation(using: .png, properties: [:])!.write(to: target)
    print(target.path)
}
