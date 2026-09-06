import AVFoundation

/// The record-start pop: the iOS counterpart of the Mac's `NSSound("Pop")`.
///
/// Synthesised rather than shipped. iOS has no named system sound to borrow the way
/// macOS does, Apple's sound files are not ours to bundle, and a hundred milliseconds
/// of tone is less code than an asset catalog entry. The shape is the Mac pop's: a
/// short percussive blip that falls in pitch and dies fast, loud enough to hear over
/// a room and short enough that it is over before the first word.
///
/// Plays through the shared audio session, so it comes out of whatever the dictation
/// is using — the speaker, or the AirPods that are also the microphone.
final class Chime {
    static let shared = Chime()

    private var player: AVAudioPlayer?

    /// How long the pop is audible. The recorder mutes its input for this long after
    /// the pop starts, so the microphone does not hand Whisper the pop itself.
    static let duration: TimeInterval = 0.12

    private init() {
        player = try? AVAudioPlayer(data: Self.render(), fileTypeHint: AVFileType.wav.rawValue)
        player?.volume = 0.9
        player?.prepareToPlay()
    }

    func playStart() {
        guard let player else { return }
        player.currentTime = 0
        player.play()
    }

    /// A falling sine with a fast exponential decay — the pop — as 16 kHz WAV data,
    /// made with the same writer the recorder uses for Whisper.
    private static func render() -> Data {
        let rate = Recorder.sampleRate
        let count = Int(rate * duration)
        var samples = [Float](repeating: 0, count: count)
        var phase = 0.0
        for index in 0..<count {
            let t = Double(index) / rate
            let progress = t / duration
            // 1400 Hz sliding to 650 Hz: the fall is what makes it a pop, not a beep.
            let frequency = 1400 - (1400 - 650) * progress
            phase += 2 * .pi * frequency / rate
            // 3 ms attack so it does not click, then a 28 ms decay.
            let attack = min(1, t / 0.003)
            let decay = exp(-t / 0.028)
            let body = sin(phase) + 0.25 * sin(2 * phase)
            samples[index] = Float(0.55 * attack * decay * body)
        }
        return Recorder.wav(from: samples)
    }
}
