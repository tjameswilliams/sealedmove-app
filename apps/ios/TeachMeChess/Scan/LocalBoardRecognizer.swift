import CoreML
import UIKit
import Vision

/// The on-device recognizer: BoardLocator finds the grid, a bundled 13-class
/// CoreML model (BoardVision.mlpackage, trained in ml/boardvision) classifies
/// the 64 squares, and Vision OCR reads any "White/Black to move" caption.
/// Nothing leaves the device and there is no per-scan cost.
struct LocalBoardRecognizer: BoardRecognizer {
    enum LocalError: LocalizedError {
        case modelMissing
        case badImage
        case noBoardFound

        var errorDescription: String? {
            switch self {
            case .modelMissing:
                return "The on-device recognition model isn't bundled in this build."
            case .badImage:
                return "That image couldn't be read."
            case .noBoardFound:
                return "No board grid found in that photo. Crop closer to the "
                    + "diagram, or set the position up by hand below."
            }
        }
    }

    /// Below this comb strength the located "grid" is likely noise.
    static let minimumConfidence = 2.0
    /// Squares classified below this probability get counted in the notes.
    static let shakyProbability = 0.8

    static var isAvailable: Bool {
        Bundle.main.url(forResource: "BoardVision", withExtension: "mlmodelc") != nil
    }

    func recognize(_ image: UIImage) async throws -> RecognizedPosition {
        guard let cgImage = image.cgImage else { throw LocalError.badImage }
        let model = try Self.loadModel()

        guard let board = BoardLocator.locate(in: cgImage),
              board.confidence >= Self.minimumConfidence,
              let crop = cgImage.cropping(to: board.rect)
        else { throw LocalError.noBoardFound }

        let (boardField, shaky) = try Self.classify(board: crop, model: model)
        let whiteToMove = Self.sideToMove(in: cgImage)

        var notes: [String] = []
        if shaky > 0 {
            notes.append("\(shaky) square\(shaky == 1 ? "" : "s") read with low "
                + "confidence. Check the board against the photo.")
        }
        if whiteToMove != nil {
            notes.append("Side to move read from the caption.")
        }
        return RecognizedPosition(
            boardField: boardField,
            whiteToMove: whiteToMove,
            notes: notes.isEmpty ? nil : notes.joined(separator: " "))
    }

    // MARK: - Classification

    private static func loadModel() throws -> VNCoreMLModel {
        guard let url = Bundle.main.url(forResource: "BoardVision",
                                        withExtension: "mlmodelc"),
              let mlModel = try? MLModel(contentsOf: url),
              let vnModel = try? VNCoreMLModel(for: mlModel)
        else { throw LocalError.modelMissing }
        return vnModel
    }

    /// Slice the located board into 64 tiles and classify each. Returns the
    /// FEN board field plus how many squares came back under the confidence
    /// bar. Tile rows top-to-bottom are ranks 8 to 1, matching training.
    private static func classify(
        board: CGImage, model: VNCoreMLModel
    ) throws -> (boardField: String, shaky: Int) {
        // Resample the board once so every tile is an exact 1/8 slice.
        let side = 512
        guard let resized = resize(board, to: side) else { throw LocalError.badImage }
        let tile = side / 8

        var ranks: [String] = []
        var shaky = 0
        for row in 0..<8 {
            var rank = ""
            var run = 0
            for file in 0..<8 {
                let rect = CGRect(x: file * tile, y: row * tile,
                                  width: tile, height: tile)
                guard let square = resized.cropping(to: rect) else {
                    run += 1
                    continue
                }
                let (label, prob) = classifyTile(square, model: model)
                if prob < shakyProbability { shaky += 1 }
                if label == "x" {
                    run += 1
                } else {
                    if run > 0 { rank += String(run); run = 0 }
                    rank += label
                }
            }
            if run > 0 { rank += String(run) }
            ranks.append(rank)
        }
        return (ranks.joined(separator: "/"), shaky)
    }

    private static func classifyTile(
        _ tile: CGImage, model: VNCoreMLModel
    ) -> (label: String, probability: Double) {
        let request = VNCoreMLRequest(model: model)
        request.imageCropAndScaleOption = .scaleFill
        let handler = VNImageRequestHandler(cgImage: tile)
        guard (try? handler.perform([request])) != nil,
              let top = (request.results as? [VNClassificationObservation])?.first
        else { return ("x", 0) }
        return (top.identifier, Double(top.confidence))
    }

    private static func resize(_ image: CGImage, to side: Int) -> CGImage? {
        guard let ctx = CGContext(
            data: nil, width: side, height: side,
            bitsPerComponent: 8, bytesPerRow: side * 4,
            space: CGColorSpaceCreateDeviceRGB(),
            bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)
        else { return nil }
        ctx.interpolationQuality = .high
        ctx.draw(image, in: CGRect(x: 0, y: 0, width: side, height: side))
        return ctx.makeImage()
    }

    // MARK: - Side to move (caption OCR)

    /// Reads printed cues like "White to move" around the diagram. English
    /// only for now; nil when the photo doesn't say.
    private static func sideToMove(in image: CGImage) -> Bool? {
        let request = VNRecognizeTextRequest()
        request.recognitionLevel = .fast
        request.usesLanguageCorrection = false
        let handler = VNImageRequestHandler(cgImage: image)
        guard (try? handler.perform([request])) != nil,
              let observations = request.results
        else { return nil }
        let text = observations
            .compactMap { $0.topCandidates(1).first?.string.lowercased() }
            .joined(separator: " ")
        let whiteCue = ["white to move", "white to play", "white mates"]
            .contains { text.contains($0) }
        let blackCue = ["black to move", "black to play", "black mates"]
            .contains { text.contains($0) }
        if whiteCue != blackCue { return whiteCue }
        return nil
    }
}
