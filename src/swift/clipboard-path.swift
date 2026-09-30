import AppKit
import Foundation
let args = CommandLine.arguments
let pb = NSPasteboard.general
if args.count == 2 && args[1] == "count" {
    print(pb.changeCount)
} else if args.count == 4 && args[1] == "set-if", let expected = Int(args[2]) {
    if pb.changeCount == expected {
        pb.clearContents()
        guard pb.setString(args[3], forType: .string) else { exit(1) }
        print("copied")
    } else {
        print("preserved")
    }
} else {
    fputs("Usage: clipboard-path count | set-if CHANGE_COUNT TEXT\n", stderr)
    exit(2)
}
