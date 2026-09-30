import AppKit
import Foundation
let args = CommandLine.arguments
let command = args.count > 1 ? args[1] : ""
let namedIndex = command == "watch" ? 2 : 4
let pb = args.count > namedIndex ? NSPasteboard(name: NSPasteboard.Name(args[namedIndex])) : .general
func hasImage() -> Bool {
    pb.availableType(from: [.png, .tiff, NSPasteboard.PasteboardType("public.jpeg")]) != nil
}
if command == "watch" {
    var previous = pb.changeCount
    print("{\"ready\":true}"); fflush(stdout)
    while true {
        autoreleasepool {
            let current = pb.changeCount
            if current != previous {
                previous = current
                print("{\"count\":\(current),\"image\":\(hasImage() ? "true" : "false")}")
                fflush(stdout)
            }
        }
        Thread.sleep(forTimeInterval: 0.3)
    }
} else if command == "capture", args.count >= 4, let expected = Int(args[2]) {
    guard pb.changeCount == expected else { exit(3) }
    var png: Data?
    for type in [NSPasteboard.PasteboardType.png, .tiff, NSPasteboard.PasteboardType("public.jpeg")] {
        if let data = pb.data(forType: type), let bitmap = NSBitmapImageRep(data: data) {
            png = type == .png ? data : bitmap.representation(using: .png, properties: [:])
            if png != nil { break }
        }
    }
    guard let image = png else { exit(4) }
    guard pb.changeCount == expected else { exit(3) }
    do { try image.write(to: URL(fileURLWithPath: args[3]), options: .atomic) }
    catch { fputs("\(error)\n", stderr); exit(1) }
} else {
    fputs("Usage: clipboard-watch watch [PASTEBOARD] | capture COUNT OUTPUT [PASTEBOARD]\n", stderr)
    exit(2)
}
