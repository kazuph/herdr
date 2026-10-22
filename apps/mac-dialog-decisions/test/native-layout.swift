// Concatenate after dialog.swift declarations (before MARK: app) and compile
// as main.swift. Exercises the actual AppKit drawing and window controllers.
let testApp = NSApplication.shared
testApp.setActivationPolicy(.accessory)
let output = CommandLine.arguments[1]
let samples = ["ASCII", "日本語", "e\u{301}", "👩‍💻", "┌─┬─┐", "☕️"]
let widths = [5, 6, 1, 4, 5, 2]
for (sample, width) in zip(samples, widths) {
  precondition(TerminalWidth.cells(sample) == width, "cell width: \(sample)")
}
var rasterEvidence: [[String: Any]] = []
for appearance in [NSAppearance.Name.aqua, .darkAqua] {
  testApp.appearance = NSAppearance(named: appearance)
  let lines = samples.map { "│" + $0 + String(repeating: " ", count: 12 - TerminalWidth.cells($0)) + "│" }
  let view = TerminalPreview(text: lines.joined(separator: "\n"))
  view.appearance = testApp.appearance
  let bitmap = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: Int(ceil(view.frame.width)), pixelsHigh: Int(ceil(view.frame.height)), bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)!
  NSGraphicsContext.saveGraphicsState()
  NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: bitmap)
  let transform = NSAffineTransform()
  transform.translateX(by: 0, yBy: CGFloat(bitmap.pixelsHigh))
  transform.scaleX(by: 1, yBy: -1)
  transform.concat()
  view.effectiveAppearance.performAsCurrentDrawingAppearance { view.draw(view.bounds) }
  NSGraphicsContext.restoreGraphicsState()
  let data = bitmap.representation(using: .png, properties: [:])!
  try data.write(to: URL(fileURLWithPath: "\(output)/preview-\(appearance.rawValue).png"))
  // Compare the actual raster columns of the right border. Every sample is
  // padded to twelve cells using the Rust reader's width contract.
  let start = Int(13 * view.cell)
  var columns: [[Int]] = []
  for row in lines.indices {
    let range = Int(CGFloat(row) * view.rowHeight)..<min(bitmap.pixelsHigh, Int(CGFloat(row + 1) * view.rowHeight))
    columns.append((start..<bitmap.pixelsWide).filter { x in
      range.contains { y in (bitmap.colorAt(x: x, y: y)?.alphaComponent ?? 0) > 0 }
    })
  }
  precondition(columns.allSatisfy { $0 == columns[0] && !$0.isEmpty }, "border raster columns: \(columns)")
  rasterEvidence.append(["appearance": appearance.rawValue, "cell_px": view.cell, "right_border_columns": columns])
}
let json = try JSONSerialization.data(withJSONObject: rasterEvidence, options: [.prettyPrinted, .sortedKeys])
try json.write(to: URL(fileURLWithPath: "\(output)/preview-layout.json"))
print("PASS: scalar widths and actual raster border columns in light/dark")
