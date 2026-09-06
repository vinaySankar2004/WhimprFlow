import CoreGraphics
import Foundation

/// Turns a finger path over the letter keys into a word.
///
/// The method is the one every glide keyboard descends from (SHARK², Kristensson &
/// Zhai): each candidate word has an *ideal* path — the polyline through its letters'
/// key centres — and the word whose ideal path is closest in shape to what the finger
/// drew wins, with word frequency breaking ties. Two channels, both cheap:
///
/// - **shape**: both paths resampled to the same number of points along their length,
///   then the mean point-to-point distance, in key widths;
/// - **location**: every letter's key centre must lie within reach of the drawn path,
///   in order, or the word is out — this is what makes "hello" beat "halo" when the
///   finger visibly went through `l`.
///
/// The vocabulary is `words-20k.txt`, most frequent first; its order is the prior.
/// Nothing here learns or phones home.
final class SwipeDecoder {
    struct Candidate {
        let word: String
        let score: Double
    }

    /// Words by first letter, in frequency order, as arrays of key indices (a=0…z=25).
    private var byFirstLetter: [[(word: String, letters: [Int], rank: Int)]] = Array(repeating: [], count: 26)
    private(set) var isLoaded = false

    /// How many points both paths are resampled to. Enough to follow a long word's
    /// corners, few enough that a thousand candidates score in a few milliseconds.
    private let samples = 40

    init() {}

    /// Load the vocabulary. Done on first use rather than at keyboard launch, which
    /// has a tight budget and is on the critical path of every keyboard switch.
    func loadIfNeeded() {
        guard !isLoaded else { return }
        isLoaded = true
        guard let url = Bundle.main.url(forResource: "words-20k", withExtension: "txt"),
              let text = try? String(contentsOf: url, encoding: .utf8) else { return }
        var rank = 0
        for line in text.split(separator: "\n") {
            let word = line.trimmingCharacters(in: .whitespaces)
            // Two letters or more, letters only: single letters are typed, and a
            // path cannot spell an apostrophe.
            guard word.count >= 2 else { continue }
            var letters: [Int] = []
            var ok = true
            for scalar in word.unicodeScalars {
                guard scalar.value >= 97, scalar.value <= 122 else { ok = false; break }
                letters.append(Int(scalar.value) - 97)
            }
            guard ok else { continue }
            rank += 1
            byFirstLetter[letters[0]].append((word, letters, rank))
        }
    }

    /// Decode a path. `centres` maps key index (a=0…z=25) to the key's centre; `keyWidth`
    /// normalises distances. Returns the best few, best first, or empty when nothing
    /// is close enough to be worth inserting.
    func decode(path: [CGPoint], centres: [Int: CGPoint], keyWidth: CGFloat) -> [Candidate] {
        loadIfNeeded()
        guard path.count >= 2, keyWidth > 0 else { return [] }

        let drawn = resample(path, to: samples)
        guard let first = nearestKey(to: path[0], centres: centres),
              let last = nearestKey(to: path[path.count - 1], centres: centres) else { return [] }

        // Letters the path passed near, in order, for the location check. Generous
        // radius: a thumb is wide and the path clips corners.
        let reach = Double(keyWidth) * 1.35
        let width = Double(keyWidth)

        // The last letter may be the key under the finger or one it just left —
        // people lift a little late or early.
        var lastAllowed: Set<Int> = [last]
        if let lastCentre = centres[last] {
            for (key, centre) in centres where hypot(Double(centre.x - lastCentre.x), Double(centre.y - lastCentre.y)) < width * 1.1 {
                lastAllowed.insert(key)
            }
        }

        var results: [Candidate] = []
        results.reserveCapacity(16)

        for entry in byFirstLetter[first] {
            guard lastAllowed.contains(entry.letters[entry.letters.count - 1]) else { continue }
            // Location channel: each letter's centre within reach of the path, in
            // order. A repeated letter ("hello") needs no separate path point.
            var cursor = 0
            var passes = true
            var previous = -1
            for letter in entry.letters {
                if letter == previous { continue }
                previous = letter
                guard let centre = centres[letter] else { passes = false; break }
                var found = false
                var index = cursor
                while index < path.count {
                    let point = path[index]
                    if hypot(Double(point.x - centre.x), Double(point.y - centre.y)) <= reach {
                        found = true
                        cursor = index
                        break
                    }
                    index += 1
                }
                if !found { passes = false; break }
            }
            guard passes else { continue }

            // Shape channel.
            var ideal: [CGPoint] = []
            ideal.reserveCapacity(entry.letters.count)
            previous = -1
            for letter in entry.letters where letter != previous {
                previous = letter
                if let centre = centres[letter] { ideal.append(centre) }
            }
            let idealResampled = resample(ideal, to: samples)
            var total = 0.0
            for index in 0..<samples {
                let a = drawn[index]
                let b = idealResampled[index]
                total += hypot(Double(a.x - b.x), Double(a.y - b.y))
            }
            let shape = total / Double(samples) / width

            // Frequency prior: a gentle log so the shape still decides between two
            // plausible words, and only a near-tie goes to the commoner one.
            let prior = log(Double(entry.rank) + 1) * 0.045
            // Length prior: a path through five keys is not a two-letter word.
            let visited = Double(distinctKeysVisited(path, centres: centres, reach: width * 0.75))
            let lengthPenalty = max(0, visited - Double(entry.letters.count) - 1) * 0.25

            results.append(Candidate(word: entry.word, score: shape + prior + lengthPenalty))
        }

        results.sort { $0.score < $1.score }
        // Beyond about a key and a half of average deviation nothing is a match;
        // inserting it would be typing a word the user did not draw.
        return Array(results.prefix(4)).filter { $0.score < 1.6 }
    }

    // MARK: - Geometry

    private func nearestKey(to point: CGPoint, centres: [Int: CGPoint]) -> Int? {
        var best: (Int, Double)?
        for (key, centre) in centres {
            let distance = hypot(Double(point.x - centre.x), Double(point.y - centre.y))
            if best == nil || distance < best!.1 { best = (key, distance) }
        }
        return best?.0
    }

    private func distinctKeysVisited(_ path: [CGPoint], centres: [Int: CGPoint], reach: Double) -> Int {
        var visited: [Int] = []
        for point in path {
            guard let key = nearestKey(to: point, centres: centres), let centre = centres[key] else { continue }
            guard hypot(Double(point.x - centre.x), Double(point.y - centre.y)) <= reach else { continue }
            if visited.last != key { visited.append(key) }
        }
        return visited.count
    }

    /// `count` points spaced evenly along the polyline's length. A one-point path
    /// (a word of one distinct letter) becomes `count` copies of it.
    private func resample(_ points: [CGPoint], to count: Int) -> [CGPoint] {
        guard points.count > 1 else { return Array(repeating: points.first ?? .zero, count: count) }
        var lengths: [Double] = [0]
        for index in 1..<points.count {
            let a = points[index - 1], b = points[index]
            lengths.append(lengths[index - 1] + hypot(Double(b.x - a.x), Double(b.y - a.y)))
        }
        let total = lengths[lengths.count - 1]
        guard total > 0 else { return Array(repeating: points[0], count: count) }
        var out: [CGPoint] = []
        out.reserveCapacity(count)
        var segment = 1
        for index in 0..<count {
            let target = total * Double(index) / Double(count - 1)
            while segment < points.count - 1, lengths[segment] < target { segment += 1 }
            let a = points[segment - 1], b = points[segment]
            let span = lengths[segment] - lengths[segment - 1]
            let t = span > 0 ? (target - lengths[segment - 1]) / span : 0
            out.append(CGPoint(x: a.x + (b.x - a.x) * CGFloat(t), y: a.y + (b.y - a.y) * CGFloat(t)))
        }
        return out
    }
}
