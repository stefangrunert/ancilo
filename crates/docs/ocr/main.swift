// ancilo-ocr – text recognition for scans and photos, with the system's own
// Vision framework: on this computer, no network.
//
//   ancilo-ocr <file> [max-pages]
//
// Reads an image (JPEG, PNG, HEIC, TIFF, WebP …) or a PDF (each page rendered)
// and prints JSON on stdout: {"pages": ["text of page 1", …]}. Errors go to
// stderr with exit code 1.

import CoreGraphics
import Foundation
import ImageIO
import Vision

func fail(_ message: String) -> Never {
    FileHandle.standardError.write(("error: " + message + "\n").data(using: .utf8)!)
    exit(1)
}

/// Draws into a white RGB canvas of `width`×`height` – transparent parts
/// become paper, not black.
func canvas(_ width: Int, _ height: Int, draw: (CGContext) -> Void) -> CGImage? {
    guard width > 0, height > 0, let ctx = CGContext(
        data: nil, width: width, height: height, bitsPerComponent: 8, bytesPerRow: 0,
        space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue)
    else { return nil }
    ctx.setFillColor(CGColor(red: 1, green: 1, blue: 1, alpha: 1))
    ctx.fill(CGRect(x: 0, y: 0, width: width, height: height))
    ctx.interpolationQuality = .high
    draw(ctx)
    return ctx.makeImage()
}

/// The scale that brings the long side to at least 2000 px (small text gets
/// readable) and at most 4000 px (memory stays bounded).
func scaleFor(_ width: Double, _ height: Double) -> Double {
    let long = max(width, height)
    return min(max(1.0, 2000.0 / long), 4000.0 / long)
}

/// The languages to read, in order: the user's own first, then the common
/// Latin-script ones.
let languages: [String] = {
    var list = Locale.preferredLanguages.map { $0.replacingOccurrences(of: "_", with: "-") }
    list += ["de-DE", "en-US", "fr-FR", "it-IT", "es-ES", "pt-BR", "nl-NL"]
    var seen = Set<String>()
    return list.filter { seen.insert($0).inserted }
}()

/// The text Vision finds in one image, in reading order.
func recognize(_ image: CGImage, _ orientation: CGImagePropertyOrientation = .up) -> String {
    let request = VNRecognizeTextRequest()
    request.recognitionLevel = .accurate
    request.usesLanguageCorrection = true
    // Latin-script languages first: left to itself, Vision sometimes takes
    // a receipt for Cyrillic.
    let supported = (try? request.supportedRecognitionLanguages()) ?? []
    request.recognitionLanguages = languages.filter { supported.contains($0) }
    let handler = VNImageRequestHandler(cgImage: image, orientation: orientation, options: [:])
    do {
        try handler.perform([request])
    } catch {
        fail("text recognition failed: \(error.localizedDescription)")
    }
    let lines = (request.results ?? []).compactMap { $0.topCandidates(1).first?.string }
    return lines.joined(separator: "\n")
}

/// A PDF page as an image (at least ~200 dpi, at most 4000 px).
func render(_ page: CGPDFPage) -> CGImage? {
    let box = page.getBoxRect(.mediaBox)
    guard box.width > 0, box.height > 0 else { return nil }
    let scale = min(max(200.0 / 72.0, scaleFor(box.width, box.height)), 4000.0 / max(box.width, box.height))
    return canvas(Int(box.width * scale), Int(box.height * scale)) { ctx in
        ctx.scaleBy(x: scale, y: scale)
        ctx.translateBy(x: -box.minX, y: -box.minY)
        ctx.drawPDFPage(page)
    }
}

/// A photo or scan on white paper, sized for reading.
func prepare(_ image: CGImage) -> CGImage? {
    let scale = scaleFor(Double(image.width), Double(image.height))
    let width = Int(Double(image.width) * scale), height = Int(Double(image.height) * scale)
    return canvas(width, height) { ctx in
        ctx.draw(image, in: CGRect(x: 0, y: 0, width: width, height: height))
    }
}

let args = CommandLine.arguments
guard args.count >= 2 else { fail("usage: ancilo-ocr <file> [max-pages]") }
let url = URL(fileURLWithPath: args[1])
let maxPages = args.count >= 3 ? max(1, Int(args[2]) ?? 50) : 50
var pages: [String] = []

if url.pathExtension.lowercased() == "pdf" {
    guard let doc = CGPDFDocument(url as CFURL) else { fail("cannot open this PDF") }
    if doc.isEncrypted && !doc.unlockWithPassword("") { fail("this PDF is protected by a password") }
    if doc.numberOfPages > 0 {
        for i in 1...min(doc.numberOfPages, maxPages) {
            let text = autoreleasepool { () -> String in
                guard let page = doc.page(at: i), let image = render(page) else { return "" }
                return recognize(image)
            }
            pages.append(text)
        }
    }
} else {
    guard let source = CGImageSourceCreateWithURL(url as CFURL, nil),
          CGImageSourceGetCount(source) > 0
    else { fail("cannot open this image") }
    for i in 0..<min(CGImageSourceGetCount(source), maxPages) {
        let text = autoreleasepool { () -> String in
            guard let raw = CGImageSourceCreateImageAtIndex(source, i, nil), let image = prepare(raw)
            else { return "" }
            // Photos come as taken: Vision turns them the way they were held.
            let props = CGImageSourceCopyPropertiesAtIndex(source, i, nil) as? [CFString: Any]
            let value = (props?[kCGImagePropertyOrientation] as? UInt32) ?? 1
            return recognize(image, CGImagePropertyOrientation(rawValue: value) ?? .up)
        }
        pages.append(text)
    }
}

let out = try! JSONSerialization.data(withJSONObject: ["pages": pages], options: [])
FileHandle.standardOutput.write(out)
FileHandle.standardOutput.write("\n".data(using: .utf8)!)
