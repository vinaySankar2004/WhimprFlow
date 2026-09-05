import AVFoundation
import Foundation

/// Microphone capture, resampled to the 16 kHz mono float PCM Whisper wants.
///
/// # Why this is a port and not a bridge call
///
/// Everything else that shapes the output goes through `whimpr-core`. This does not:
/// it works on buffers of tens of thousands of floats, and marshalling those through
/// a JSON bridge would cost more than the DSP. The constants below are therefore
/// *copies*, and each one names the Rust it was copied from — change one there and
/// this file needs the same change.
@Observable
final class Recorder {
    /// Whisper's rate. Anything else has to be resampled before it is sent.
    static let sampleRate: Double = 16_000

    private let engine = AVAudioEngine()
    private var converter: AVAudioConverter?
    private var samples: [Float] = []
    private let lock = NSLock()

    /// 0…1, already log-scaled for display. See `level(from:)`.
    private(set) var level: Float = 0

    /// Whether the engine is running at all — which is not the same as recording.
    ///
    /// The engine runs in **standby** whenever the app wants to stay reachable from
    /// the keyboard, discarding everything it hears. `UIBackgroundModes: audio` keeps
    /// an app alive only while audio is *actively* running; declaring the mode and
    /// then idling gets you suspended within seconds of backgrounding, which is what
    /// made the keyboard's mic key always fall back to opening the app.
    private(set) var isEngineRunning = false

    /// Whether the samples arriving are being kept.
    private(set) var isCapturing = false

    enum Failure: LocalizedError {
        case denied
        case sessionFailed(String)

        var errorDescription: String? {
            switch self {
            case .denied: return "WhimprFlow needs microphone access to dictate."
            case let .sessionFailed(m): return "Could not start the microphone: \(m)"
            }
        }
    }

    // MARK: - Permission

    static func requestPermission() async -> Bool {
        await withCheckedContinuation { continuation in
            AVAudioApplication.requestRecordPermission { continuation.resume(returning: $0) }
        }
    }

    static var hasPermission: Bool {
        AVAudioApplication.shared.recordPermission == .granted
    }

    // MARK: - Session

    /// Configure the shared audio session for recording.
    ///
    /// `.mixWithOthers` matters more than it looks: dictating while something else
    /// plays should duck that audio, not stop it, and without this a dictation
    /// interrupts whatever the user was listening to.
    func activateSession() throws {
        let session = AVAudioSession.sharedInstance()
        do {
            try session.setCategory(
                .playAndRecord,
                mode: .measurement,
                options: [.mixWithOthers, .allowBluetooth, .defaultToSpeaker]
            )
            try session.setActive(true)
        } catch {
            throw Failure.sessionFailed(error.localizedDescription)
        }
    }

    func deactivateSession() {
        try? AVAudioSession.sharedInstance().setActive(false, options: .notifyOthersOnDeactivation)
    }

    // MARK: - Capture

    /// Start the engine in standby: capturing from the microphone and throwing it
    /// away, so the app stays alive in the background and the mic key can reach it
    /// without a visible app switch.
    ///
    /// The orange microphone indicator is on for as long as this runs. That is not
    /// avoidable and should not be hidden — an app holding the mic open is exactly
    /// what this is.
    func startEngine() throws {
        guard !isEngineRunning else { return }
        guard Self.hasPermission else { throw Failure.denied }
        try activateSession()

        let input = engine.inputNode
        // The hardware format, whatever it happens to be. Never assume: a Bluetooth
        // headset that switches to its hands-free profile mid-call changes rate,
        // channel count *and* sample format, and a hard-coded format stops working
        // exactly when someone is on a call.
        let inputFormat = input.outputFormat(forBus: 0)
        guard let target = AVAudioFormat(
            commonFormat: .pcmFormatFloat32,
            sampleRate: Self.sampleRate,
            channels: 1,
            interleaved: false
        ) else {
            throw Failure.sessionFailed("could not build the 16 kHz target format")
        }
        converter = AVAudioConverter(from: inputFormat, to: target)

        input.removeTap(onBus: 0)
        input.installTap(onBus: 0, bufferSize: 4096, format: inputFormat) { [weak self] buffer, _ in
            self?.accept(buffer, target: target)
        }

        engine.prepare()
        do {
            try engine.start()
        } catch {
            throw Failure.sessionFailed(error.localizedDescription)
        }
        isEngineRunning = true
    }

    /// Tear the engine down completely. Releases the microphone and lets iOS suspend
    /// the app, after which the keyboard has to open it to dictate.
    func stopEngine() {
        guard isEngineRunning else { return }
        engine.inputNode.removeTap(onBus: 0)
        engine.stop()
        isEngineRunning = false
        isCapturing = false
        level = 0
        LevelChannel.shared.level = 0
        deactivateSession()
    }

    /// Start keeping what the microphone hears. Starts the engine first if it is not
    /// already in standby, so a dictation works whether or not standby is enabled.
    func beginCapture() throws {
        try startEngine()
        lock.withLock { samples.removeAll(keepingCapacity: true) }
        isCapturing = true
    }

    /// Stop keeping samples and return the recording, normalized and ready to send.
    ///
    /// Leaves the engine running: dropping back to standby rather than stopping is
    /// what keeps the app reachable for the *next* dictation. Call `stopEngine()` to
    /// actually release the microphone.
    func endCapture() -> [Float] {
        guard isCapturing else { return [] }
        isCapturing = false
        level = 0
        // Or the keyboard's waveform freezes at the last thing it heard.
        LevelChannel.shared.level = 0

        var captured = lock.withLock { samples }
        Self.normalizeForASR(&captured)
        return captured
    }

    private func accept(_ buffer: AVAudioPCMBuffer, target: AVAudioFormat) {
        // Standby: the engine must keep running to hold the app alive, but nothing it
        // hears is kept, converted or measured. Returning here rather than not
        // installing the tap is deliberate — removing and reinstalling a tap around
        // every dictation restarts the engine, and the gap that opens while it spins
        // back up swallows the first word.
        guard isCapturing else { return }
        guard let converter else { return }
        let ratio = target.sampleRate / buffer.format.sampleRate
        let capacity = AVAudioFrameCount(Double(buffer.frameLength) * ratio) + 1024
        guard let out = AVAudioPCMBuffer(pcmFormat: target, frameCapacity: capacity) else { return }

        var consumed = false
        var error: NSError?
        converter.convert(to: out, error: &error) { _, status in
            if consumed {
                status.pointee = .noDataNow
                return nil
            }
            consumed = true
            status.pointee = .haveData
            return buffer
        }
        guard error == nil, let channel = out.floatChannelData?[0], out.frameLength > 0 else { return }

        let chunk = UnsafeBufferPointer(start: channel, count: Int(out.frameLength))
        lock.withLock { samples.append(contentsOf: chunk) }

        let meter = Self.level(from: chunk)
        // Straight to shared memory from the audio thread — a store, no allocation and
        // nothing to await, so the keyboard's waveform tracks the voice at the rate
        // the microphone actually delivers rather than the rate the main thread
        // happens to drain.
        LevelChannel.shared.level = meter
        Task { @MainActor [weak self] in self?.level = meter }
    }

    // MARK: - Signal shaping

    /// Display level for the waveform.
    ///
    /// Logarithmic, not `rms * k`. A linear meter put quiet speech below the idle
    /// shimmer, so speaking softly rendered as silence — the meter said nothing was
    /// happening while the recording was fine.
    private static func level(from samples: UnsafeBufferPointer<Float>) -> Float {
        guard !samples.isEmpty else { return 0 }
        var sum: Float = 0
        for sample in samples { sum += sample * sample }
        let rms = (sum / Float(samples.count)).squareRoot()
        guard rms > 0 else { return 0 }
        // -50 dBFS reads as silence, 0 dBFS as full scale.
        let db = 20 * log10(rms)
        return min(max((db + 50) / 50, 0), 1)
    }

    /// Lift a quiet recording before it is sent.
    ///
    /// Ported from `whimpr_audio::normalize_for_asr` — constants identical. Whisper
    /// *drops* soft words rather than mis-hearing them, so an un-normalized quiet
    /// recording loses its ending. The gain cap is what stops room tone being
    /// amplified into something the model hallucinates over; do not remove it.
    static func normalizeForASR(_ samples: inout [Float]) {
        /// Above this the recording is already fine; leave it alone.
        let healthyPeak: Float = 0.5
        /// What a quiet recording is lifted to. Short of 1.0 so interpolation
        /// between two near-peak samples cannot overshoot into clipping.
        let targetPeak: Float = 0.7
        /// Past roughly this much, what is being amplified is the noise floor.
        let maxGain: Float = 8.0
        /// Below this there is no signal to rescue, only room tone.
        let noiseFloor: Float = 0.002

        let peak = samples.reduce(Float(0)) { max($0, abs($1)) }
        guard peak >= noiseFloor, peak < healthyPeak else { return }
        let gain = min(targetPeak / peak, maxGain)
        for index in samples.indices { samples[index] *= gain }
    }

    // MARK: - Encoding

    /// One second of silence, appended before sending.
    ///
    /// Ported from `whimpr_asr::TAIL_PAD_SAMPLES`. Whisper will not start a segment
    /// within a second of the end of the audio, so an utterance that stops the
    /// instant the speaker does loses its last words — which is every push-to-talk
    /// recording. It looks like a model problem and is not.
    static let tailPadSamples = 16_000

    /// A 16-bit PCM WAV, with the tail padding already added.
    static func wav(from samples: [Float]) -> Data {
        let frameCount = samples.count + tailPadSamples
        let byteCount = frameCount * 2

        var data = Data(capacity: 44 + byteCount)
        func append<T: FixedWidthInteger>(_ value: T) {
            withUnsafeBytes(of: value.littleEndian) { data.append(contentsOf: $0) }
        }
        data.append(contentsOf: Array("RIFF".utf8))
        append(UInt32(36 + byteCount))
        data.append(contentsOf: Array("WAVEfmt ".utf8))
        append(UInt32(16))                       // PCM header size
        append(UInt16(1))                        // format: PCM
        append(UInt16(1))                        // channels: mono
        append(UInt32(sampleRate))
        append(UInt32(sampleRate) * 2)           // byte rate
        append(UInt16(2))                        // block align
        append(UInt16(16))                       // bits per sample
        data.append(contentsOf: Array("data".utf8))
        append(UInt32(byteCount))

        for sample in samples {
            append(Int16(max(-1, min(1, sample)) * 32767))
        }
        // The padding itself: silence, so it costs nothing but the segment boundary.
        data.append(Data(count: tailPadSamples * 2))
        return data
    }
}
