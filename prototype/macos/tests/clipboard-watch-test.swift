import AppKit
import Foundation
let pb = NSPasteboard.withUniqueName()
defer { pb.releaseGlobally() }
let dir = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: false)
defer { try? FileManager.default.removeItem(at: dir) }
guard CommandLine.arguments.count == 2 else {
    fatalError("Usage: clipboard-watch-test PATH_TO_CLIPBOARD_WATCH")
}
let helper = URL(fileURLWithPath: CommandLine.arguments[1]).standardizedFileURL.path
let watcher = Process(); watcher.executableURL = URL(fileURLWithPath: helper)
watcher.arguments = ["watch", pb.name.rawValue]
let pipe = Pipe(); watcher.standardOutput = pipe
try watcher.run()
defer { watcher.terminate(); watcher.waitUntilExit() }
let ready = String(data: pipe.fileHandleForReading.availableData, encoding: .utf8)!
precondition(ready.contains("ready"))
let bitmap = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: 2, pixelsHigh: 2, bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)!
pb.clearContents(); pb.setData(bitmap.representation(using: .png, properties: [:])!, forType: .png)
let event = String(data: pipe.fileHandleForReading.availableData, encoding: .utf8)!
precondition(event.contains("\"image\":true"))
let count = pb.changeCount
func capture(_ expected: Int, _ name: String) throws -> Int32 {
    let p = Process(); p.executableURL = URL(fileURLWithPath: helper)
    p.arguments = ["capture", String(expected), dir.appendingPathComponent(name).path, pb.name.rawValue]
    try p.run(); p.waitUntilExit(); return p.terminationStatus
}
let status = try capture(count, "image.png")
precondition(status == 0)
let captured = try Data(contentsOf: dir.appendingPathComponent("image.png"))
precondition(NSBitmapImageRep(data: captured) != nil)
pb.clearContents(); pb.setString("newer clipboard", forType: .string)
let stale = try capture(count, "stale.png")
precondition(stale == 3)
precondition(pb.string(forType: .string) == "newer clipboard")
print("PASS: real clipboard watcher, PNG capture, stale-image rejection; personal clipboard untouched")
