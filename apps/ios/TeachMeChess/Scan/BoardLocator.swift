import CoreGraphics
import UIKit

/// Finds an axis-aligned chess board grid in a 2D diagram or screenshot.
///
/// Approach (ported from the numpy board finders the open-source diagram
/// recognizers converge on): compute horizontal and vertical gradient
/// projections, then search for the 7 interior grid lines as an evenly
/// spaced comb, per axis. No OpenCV; plain pixel math on a downscaled
/// grayscale buffer. Photos of physical boards at an angle are out of
/// scope here (phase two); the editor remains the fallback.
enum BoardLocator {
    struct Result {
        /// Board bounds in the ORIGINAL image's pixel coordinates.
        let rect: CGRect
        /// Comb strength relative to the image's overall edge energy.
        /// Below ~2 the "grid" is probably noise.
        let confidence: Double
    }

    private static let maxSide = 800

    static func locate(in image: CGImage) -> Result? {
        guard let gray = grayscale(image) else { return nil }
        let w = gray.width, h = gray.height

        // Gradient projections: strong vertical lines spike colScore.
        var colScore = [Double](repeating: 0, count: w)
        var rowScore = [Double](repeating: 0, count: h)
        gray.pixels.withUnsafeBufferPointer { buf in
            for y in 0..<h {
                let row = y * w
                for x in 0..<(w - 1) {
                    let d = abs(Int(buf[row + x + 1]) - Int(buf[row + x]))
                    colScore[x] += Double(d)
                }
            }
            for y in 0..<(h - 1) {
                let row = y * w, next = (y + 1) * w
                for x in 0..<w {
                    rowScore[y] += Double(abs(Int(buf[next + x]) - Int(buf[row + x])))
                }
            }
        }

        guard let bestX = bestComb(scores: colScore, extent: w),
              let bestY = bestComb(scores: rowScore, extent: h)
        else { return nil }

        // A chess board is square: the two axes must roughly agree.
        let spacingRatio = max(bestX.spacing, bestY.spacing) / min(bestX.spacing, bestY.spacing)
        guard spacingRatio < 1.12 else { return nil }

        // The comb is periodic: a grid shifted by exactly one square still
        // matches 6 of 7 interior lines plus the board's outer edge, so the
        // line score alone can lock one rank/file off (and then a caption
        // under the diagram gets read as pieces). Only the true grid
        // alternates light/dark cells — pick the phase by checker contrast.
        let (originX, originY) = bestCheckerPhase(gray: gray, x: bestX, y: bestY)

        let scale = Double(image.width) / Double(w)
        let rect = CGRect(
            x: originX * scale,
            y: originY * scale,
            width: bestX.spacing * 8 * scale,
            height: bestY.spacing * 8 * scale)
            .intersection(CGRect(x: 0, y: 0,
                                 width: image.width, height: image.height))
        guard rect.width > 40, rect.height > 40 else { return nil }
        return Result(rect: rect, confidence: min(bestX.confidence, bestY.confidence))
    }

    // MARK: - Comb search

    private struct Comb {
        let origin: Double
        let spacing: Double
        let confidence: Double
    }

    /// Search over (origin, spacing) for the strongest 7-interior-line comb.
    /// Border lines are excluded on purpose: a board flush against the crop
    /// has no gradient at its outer edges.
    private static func bestComb(scores: [Double], extent: Int) -> Comb? {
        let n = scores.count
        let mean = scores.reduce(0, +) / Double(max(1, n))
        guard mean > 0 else { return nil }

        var best: (score: Double, origin: Int, spacing: Int)? = nil
        let minSpacing = 14, maxSpacing = extent / 8
        guard maxSpacing >= minSpacing else { return nil }

        for spacing in minSpacing...maxSpacing {
            let span = spacing * 8
            let lastOrigin = extent - span
            guard lastOrigin >= -2 else { continue }
            var origin = -2
            while origin <= lastOrigin + 2 {
                var total = 0.0
                for k in 1...7 {
                    let center = origin + k * spacing
                    var peak = 0.0
                    for dx in -2...2 {
                        let i = center + dx
                        if i >= 0 && i < n { peak = max(peak, scores[i]) }
                    }
                    total += peak
                }
                if best == nil || total > best!.score {
                    best = (total, origin, spacing)
                }
                origin += 1
            }
        }
        guard let best else { return nil }
        // 7 lines of pure mean-level gradient would score 7*mean.
        let confidence = best.score / (7 * mean)
        return Comb(origin: Double(best.origin), spacing: Double(best.spacing),
                    confidence: confidence)
    }

    // MARK: - Checker phase

    /// Try the found origin and its one-square shifts on each axis and keep
    /// the candidate whose cells alternate light/dark most strongly.
    private static func bestCheckerPhase(gray: Gray, x: Comb, y: Comb) -> (Double, Double) {
        var best = (score: -1.0, ox: x.origin, oy: y.origin)
        for dx in -1...1 {
            for dy in -1...1 {
                let ox = x.origin + Double(dx) * x.spacing
                let oy = y.origin + Double(dy) * y.spacing
                guard ox >= -2, oy >= -2,
                      ox + x.spacing * 8 <= Double(gray.width) + 2,
                      oy + y.spacing * 8 <= Double(gray.height) + 2
                else { continue }
                let score = checkerContrast(gray: gray, ox: ox, oy: oy,
                                            sx: x.spacing, sy: y.spacing)
                if score > best.score { best = (score, ox, oy) }
            }
        }
        return (best.ox, best.oy)
    }

    /// Cell patches as (x, then y) cell fractions. Pieces occupy square
    /// centers, so the CORNERS keep the square's own color — sampling
    /// centers instead makes sparse boards lose the phase vote to blank
    /// margin rows (dark pieces drag their row's contrast below an empty
    /// page row's zero).
    private static let checkerPatches: [(Double, Double, Double, Double)] = [
        (0.06, 0.22, 0.06, 0.22), (0.78, 0.94, 0.06, 0.22),
        (0.06, 0.22, 0.78, 0.94), (0.78, 0.94, 0.78, 0.94),
    ]

    /// |Σ ±(cell corner mean)| over the 8x8 grid with alternating signs:
    /// high for a real checkerboard, near zero for a grid laid over margins
    /// or text.
    private static func checkerContrast(
        gray: Gray, ox: Double, oy: Double, sx: Double, sy: Double
    ) -> Double {
        var signedSum = 0.0
        gray.pixels.withUnsafeBufferPointer { buf in
            for r in 0..<8 {
                for f in 0..<8 {
                    var total = 0.0, count = 0.0
                    for (fx0, fx1, fy0, fy1) in checkerPatches {
                        let x0 = Int(ox + (Double(f) + fx0) * sx)
                        let x1 = Int(ox + (Double(f) + fx1) * sx)
                        let y0 = Int(oy + (Double(r) + fy0) * sy)
                        let y1 = Int(oy + (Double(r) + fy1) * sy)
                        for y in max(0, y0)..<min(gray.height, max(y0 + 1, y1)) {
                            for x in max(0, x0)..<min(gray.width, max(x0 + 1, x1)) {
                                total += Double(buf[y * gray.width + x])
                                count += 1
                            }
                        }
                    }
                    guard count > 0 else { continue }
                    let sign: Double = (f + r) % 2 == 0 ? 1 : -1
                    signedSum += sign * (total / count)
                }
            }
        }
        return abs(signedSum)
    }

    // MARK: - Grayscale

    private struct Gray {
        let pixels: [UInt8]
        let width: Int
        let height: Int
    }

    private static func grayscale(_ image: CGImage) -> Gray? {
        let scale = Double(maxSide) / Double(max(image.width, image.height))
        let w = scale < 1 ? Int(Double(image.width) * scale) : image.width
        let h = scale < 1 ? Int(Double(image.height) * scale) : image.height
        var pixels = [UInt8](repeating: 0, count: w * h)
        let ok = pixels.withUnsafeMutableBytes { buf -> Bool in
            guard let ctx = CGContext(
                data: buf.baseAddress, width: w, height: h,
                bitsPerComponent: 8, bytesPerRow: w,
                space: CGColorSpaceCreateDeviceGray(),
                bitmapInfo: CGImageAlphaInfo.none.rawValue)
            else { return false }
            ctx.interpolationQuality = .medium
            ctx.draw(image, in: CGRect(x: 0, y: 0, width: w, height: h))
            return true
        }
        return ok ? Gray(pixels: pixels, width: w, height: h) : nil
    }
}
