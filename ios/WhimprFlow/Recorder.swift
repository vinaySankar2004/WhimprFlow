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

    /// Drop input until this moment. Set when the start pop plays: the speaker is a
    /// hand's width from the microphone, and without this the first thing Whisper
    /// hears is the pop. Read on the audio thread, written on the main one; a stale
    /// read costs a few milliseconds of silence at most.
    private var muteUntil: Date?

    /// Something is running that keeps the app alive.
    var isAlive: Bool { isEngineRunning || isCapturing }

    /// Whether standby *should* be up, as opposed to whether it currently is.
    ///
    /// Recovery needs the difference. `AVAudioEngine` stops itself for reasons that
    /// have nothing to do with this app — a phone call, Siri, AirPods connecting — and
    /// without a record of intent there is no way to tell "stopped because it was
    /// asked to" from "stopped and should be brought back".
    private var wantsEngine = false

    /// Audio-lifecycle observers, torn down with the recorder.
    private var observers: [NSObjectProtocol] = []

    /// Called when the engine stopped without being asked to, while a dictation was in
    /// flight. The controller decides what to do with the partial recording; this type
    /// does not have the context to.
    var onCaptureInterrupted: (() -> Void)?

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
    ///
    /// Mode `.default`, not `.measurement`. Measurement strips the system's signal
    /// processing from output as well as input, and on an iPhone that made the
    /// speaker noticeably quieter for every other app while this session was active
    /// — which in standby is all day. Whisper does not need the raw input; the user
    /// does need their volume. Confirmed by ear on the device.
    func activateSession() throws {
        let session = AVAudioSession.sharedInstance()
        do {
            try session.setCategory(
                .playAndRecord,
                mode: .default,
                options: [.mixWithOthers, .allowBluetoothHFP, .defaultToSpeaker]
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
    ///
    /// Keeping alive by playing *silence* under a `.playback` session instead, and
    /// opening the mic only for the dictation, was tried on 2026-09-05 and did not
    /// work: opening the mic from that state failed with OSStatus 560557684 (`!int`,
    /// cannotInterruptOthers — a non-mixable session activated while another app held
    /// audio; it was misread as `!cat` at the time) even with the app in the
    /// foreground, and every mic-key tap then bounced to the app. The evidence is
    /// recorded in ios/README; a second attempt should keep one category throughout
    /// and start from a device log of which call fails, not from here.
    func startEngine() throws {
        wantsEngine = true
        observeAudioLifecycle()
        try bringUpEngine()
    }

    private func bringUpEngine() throws {
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
        wantsEngine = false
        tearDownEngine()
        deactivateSession()
    }

    private func tearDownEngine() {
        guard isEngineRunning else { return }
        engine.inputNode.removeTap(onBus: 0)
        engine.stop()
        isEngineRunning = false
        isCapturing = false
        level = 0
        LevelChannel.shared.level = 0
    }

    // MARK: - Surviving interruptions

    /// Watch for everything that stops the engine without asking.
    ///
    /// This is what keeps standby — and therefore the keyboard's ability to dictate
    /// without opening the app — alive across an ordinary day. Without it a single
    /// phone call ends standby permanently: the engine stops, nothing restarts it, the
    /// app has no active audio, iOS suspends and then terminates it, and every mic-key
    /// tap from then on opens the app. The user sees an app that "randomly stops
    /// working", with no event to connect it to.
    private func observeAudioLifecycle() {
        guard observers.isEmpty else { return }
        let centre = NotificationCenter.default
        let session = AVAudioSession.sharedInstance()

        // A call, Siri, or another app taking the microphone.
        observers.append(centre.addObserver(
            forName: AVAudioSession.interruptionNotification,
            object: session,
            queue: .main
        ) { [weak self] note in
            guard let self else { return }
            let raw = note.userInfo?[AVAudioSessionInterruptionTypeKey] as? UInt ?? 0
            switch AVAudioSession.InterruptionType(rawValue: raw) {
            case .began:
                self.handleUnexpectedStop()
            case .ended:
                // The session is inactive until it is reactivated, so recovery has to
                // go through the full bring-up rather than just engine.start().
                self.recover()
            default:
                break
            }
        })

        // The route changed underneath us — AirPods connecting, a headset unplugged.
        // The engine's input format can change with it, so the tap has to be rebuilt
        // rather than the engine merely restarted.
        observers.append(centre.addObserver(
            forName: .AVAudioEngineConfigurationChange,
            object: engine,
            queue: .main
        ) { [weak self] _ in
            self?.recover()
        })

        // The audio server restarted. Every object obtained from it is stale.
        observers.append(centre.addObserver(
            forName: AVAudioSession.mediaServicesWereResetNotification,
            object: session,
            queue: .main
        ) { [weak self] _ in
            self?.handleUnexpectedStop()
            self?.recover()
        })
    }

    /// The engine went down mid-dictation. Tell the controller, which owns the
    /// decision about the partial recording.
    private func handleUnexpectedStop() {
        guard isCapturing else { return }
        onCaptureInterrupted?()
    }

    /// Put standby back, if it is supposed to be up.
    ///
    /// Rebuilds rather than restarts: after a configuration change the input node's
    /// format may differ, and a tap installed against the old one delivers nothing.
    /// Retried once after a moment because the route is sometimes still settling when
    /// the notification arrives, and a bring-up in that window throws.
    private func recover() {
        guard wantsEngine else { return }
        tearDownEngine()
        do {
            try bringUpEngine()
        } catch {
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.6) { [weak self] in
                guard let self, self.wantsEngine, !self.isEngineRunning else { return }
                try? self.bringUpEngine()
            }
        }
    }

    deinit {
        observers.forEach(NotificationCenter.default.removeObserver)
    }

    /// Start keeping what the microphone hears. Starts the engine first if it is not
    /// already in standby, so a dictation works whether or not standby is enabled.
    func beginCapture() throws {
        try startEngine()
        lock.withLock { samples.removeAll(keepingCapacity: true) }
        muteUntil = nil
        isCapturing = true
    }

    /// Ignore the microphone for `seconds` from now — see `muteUntil`.
    func muteInput(for seconds: TimeInterval) {
        muteUntil = Date().addingTimeInterval(seconds)
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
        if let muteUntil {
            if Date() < muteUntil { return }
            self.muteUntil = nil
        }
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
