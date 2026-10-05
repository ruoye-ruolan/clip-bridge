//! Native macOS pasteboard access. No helper processes or general-pasteboard tests.

use anyhow::{Context, Result, ensure};
use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::AnyObject;
use objc2::{AnyThread, MainThreadMarker};
use objc2_app_kit::{NSBitmapImageFileType, NSBitmapImageRep, NSPasteboard};
use objc2_foundation::{NSArray, NSData, NSDictionary, NSString, ns_string};

/// Read-only clipboard access: capturing an image must never replace its contents.
pub trait ClipboardAccess {
    fn change_count(&self) -> i64;
    fn has_image(&self) -> bool;
    /// Return `None` when the snapshot is stale or contains no decodable image.
    fn capture_png(&self, expected: i64) -> Result<Option<Vec<u8>>>;
}

pub struct NativeClipboard {
    board: Retained<NSPasteboard>,
    // Restrict all operations to the creating main thread, including in launchd.
    _main_thread: MainThreadMarker,
}

impl NativeClipboard {
    pub fn general() -> Result<Self> {
        let main_thread = MainThreadMarker::new()
            .context("macOS clipboard access must run on the main thread")?;
        Ok(autoreleasepool(|_| Self {
            board: NSPasteboard::generalPasteboard(),
            _main_thread: main_thread,
        }))
    }
}

fn image_types() -> [&'static NSString; 3] {
    [
        ns_string!("public.png"),
        ns_string!("public.tiff"),
        ns_string!("public.jpeg"),
    ]
}

fn encode(bitmap: &NSBitmapImageRep, format: NSBitmapImageFileType) -> Option<Retained<NSData>> {
    let properties: Retained<NSDictionary<NSString, AnyObject>> = NSDictionary::new();
    // SAFETY: The dictionary is empty, so no property keys have values of an
    // incompatible Objective-C type. AppKit accepts an empty property dictionary.
    unsafe { bitmap.representationUsingType_properties(format, &properties) }
}

impl ClipboardAccess for NativeClipboard {
    fn change_count(&self) -> i64 {
        autoreleasepool(|_| self.board.changeCount() as i64)
    }

    fn has_image(&self) -> bool {
        autoreleasepool(|_| {
            self.board
                .availableTypeFromArray(&NSArray::from_slice(&image_types()))
                .is_some()
        })
    }

    fn capture_png(&self, expected: i64) -> Result<Option<Vec<u8>>> {
        autoreleasepool(|_| {
            if self.change_count() != expected {
                return Ok(None);
            }
            for (index, kind) in image_types().iter().enumerate() {
                let Some(data) = self.board.dataForType(kind) else {
                    continue;
                };
                let Some(bitmap) = NSBitmapImageRep::initWithData(NSBitmapImageRep::alloc(), &data)
                else {
                    continue;
                };
                let png = if index == 0 {
                    // Validate PNG data, then preserve its original metadata and bytes.
                    Some(data)
                } else {
                    encode(&bitmap, NSBitmapImageFileType::PNG)
                };
                if let Some(png) = png {
                    let bytes = png.to_vec();
                    // Data providers may take time to produce or decode an image.
                    // Recheck after the final copy so a newer snapshot is skipped.
                    return Ok((self.change_count() == expected).then_some(bytes));
                }
            }
            Ok(None)
        })
    }
}

/// Exercise the real AppKit implementation using only a unique private pasteboard.
/// Run this from the executable's main thread, not a Rust test worker thread.
pub fn self_test() -> Result<()> {
    let main_thread =
        MainThreadMarker::new().context("clipboard self-test must run on the main thread")?;
    autoreleasepool(|_| {
        let private = PrivatePasteboard(NativeClipboard {
            board: NSPasteboard::pasteboardWithUniqueName(),
            _main_thread: main_thread,
        });
        let clipboard = &private.0;
        clipboard.board.clearContents();
        let baseline = clipboard.change_count();
        ensure!(
            !clipboard.has_image(),
            "new private pasteboard has an image"
        );
        ensure!(
            clipboard.capture_png(baseline)?.is_none(),
            "empty pasteboard produced an image"
        );

        // An opaque 1 x 1 PNG, embedded so the installed binary can self-test.
        const PNG: &[u8] = &[
            137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1,
            8, 2, 0, 0, 0, 144, 119, 83, 222, 0, 0, 0, 12, 73, 68, 65, 84, 120, 156, 99, 80, 104,
            120, 0, 0, 2, 68, 1, 129, 113, 194, 198, 177, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96,
            130,
        ];
        let png = NSData::with_bytes(PNG);
        let bitmap = NSBitmapImageRep::initWithData(NSBitmapImageRep::alloc(), &png)
            .context("could not decode embedded test PNG")?;
        clipboard.board.clearContents();
        ensure!(
            clipboard
                .board
                .setData_forType(Some(&png), ns_string!("public.png")),
            "could not seed private PNG pasteboard"
        );
        let image_count = clipboard.change_count();
        let original_types = clipboard.board.types();
        ensure!(
            image_count != baseline,
            "image did not change the pasteboard count"
        );
        ensure!(clipboard.has_image(), "PNG was not detected");
        ensure!(
            clipboard.capture_png(image_count)?.as_deref() == Some(PNG),
            "PNG capture did not preserve the original bytes"
        );
        ensure!(
            clipboard.capture_png(baseline)?.is_none(),
            "stale image capture was accepted"
        );
        ensure!(
            clipboard.change_count() == image_count
                && clipboard.board.types() == original_types
                && clipboard
                    .board
                    .dataForType(ns_string!("public.png"))
                    .is_some_and(|data| data.to_vec() == PNG),
            "PNG capture modified the original clipboard image, type or change count"
        );

        for (format, kind) in [
            (NSBitmapImageFileType::TIFF, ns_string!("public.tiff")),
            (NSBitmapImageFileType::JPEG, ns_string!("public.jpeg")),
        ] {
            let image = encode(&bitmap, format).context("could not encode test image")?;
            clipboard.board.clearContents();
            ensure!(
                clipboard.board.setData_forType(Some(&image), kind),
                "could not seed converted test image"
            );
            ensure!(clipboard.has_image(), "TIFF/JPEG image was not detected");
            let count = clipboard.change_count();
            let original_types = clipboard.board.types();
            let captured = clipboard
                .capture_png(count)?
                .context("TIFF/JPEG image capture failed")?;
            ensure!(captured.starts_with(&PNG[..8]), "capture is not PNG data");
            let decoded = NSBitmapImageRep::initWithData(
                NSBitmapImageRep::alloc(),
                &NSData::with_bytes(&captured),
            )
            .context("converted image is not decodable")?;
            ensure!(
                decoded.pixelsWide() == 1 && decoded.pixelsHigh() == 1,
                "image dimensions changed during conversion"
            );
            ensure!(
                clipboard.change_count() == count
                    && clipboard.board.types() == original_types
                    && clipboard
                        .board
                        .dataForType(kind)
                        .is_some_and(|data| data.to_vec() == image.to_vec()),
                "conversion modified the original clipboard image, type or change count"
            );
        }

        clipboard.board.clearContents();
        ensure!(
            clipboard.board.setData_forType(
                Some(&NSData::with_bytes(b"invalid PNG")),
                ns_string!("public.png")
            ),
            "could not seed invalid image data"
        );
        ensure!(
            clipboard.capture_png(clipboard.change_count())?.is_none(),
            "invalid PNG was accepted"
        );

        clipboard.board.clearContents();
        let text_type = ns_string!("public.utf8-plain-text");
        let newer_text = ns_string!("newer clipboard text");
        ensure!(
            clipboard.board.setString_forType(newer_text, text_type),
            "could not seed newer text"
        );
        ensure!(
            !clipboard.has_image(),
            "plain text was detected as an image"
        );
        let text_count = clipboard.change_count();
        let text_types = clipboard.board.types();
        ensure!(
            clipboard.capture_png(image_count)?.is_none(),
            "stale capture accepted newer text"
        );
        ensure!(
            clipboard.change_count() == text_count
                && clipboard.board.types() == text_types
                && clipboard.board.stringForType(text_type).as_deref() == Some(newer_text),
            "stale capture modified newer clipboard text, type or change count"
        );
        Ok(())
    })
}

struct PrivatePasteboard(NativeClipboard);

impl Drop for PrivatePasteboard {
    fn drop(&mut self) {
        self.0.board.clearContents();
        // SAFETY: releaseGlobally is an NSPasteboard instance method returning
        // void. This guard exclusively owns a unique, private test pasteboard;
        // no general/system pasteboard can reach this cleanup path.
        unsafe { objc2::msg_send![&*self.0.board, releaseGlobally] }
    }
}
