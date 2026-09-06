import AVFoundation
import Foundation
import Observation
import UIKit

/// The dictation loop: record, recognize, clean, publish.
///
/// This is the iOS counterpart of `hotkey.rs`, and like it, it decides nothing about
/// the text. Recognition and cleanup are network calls; everything between and around
/// them is `whimpr-core` through the bridge. The order in particular is not
/// reimplemented here — `prepare` and `finish` own it.
@MainActor
@Observable
final class DictationController {
    static let shared = DictationController()

    enum Phase: Equatable {
        case idle
        case recording
        case transcribing
        /// Terminal states linger briefly so they can be seen; the view drives that,
        /// not this type — the same reason the Mac's shell lingers rather than
        /// emitting a terminal bar state followed immediately by `Idle`.
        case done(String)
        case failed(String)
    }

    private(set) var phase: Phase = .idle
    private(set) var lastEngine: Engine?
    private(set) var lastDegraded: String?

    let recorder = Recorder()
    private var settings: Settings { .shared }
    private var heartbeat: Task<Void, Never>?
    private let activity = StandbyActivityController()

    /// The idle clock. Standby releases the mic when this fires; every dictation and
    /// every foreground visit winds it back to the full timeout.
    private var idleTimer: Task<Void, Never>?
    /// When standby will release the mic if nothing happens. Nil when there is no
    /// limit or standby is not up. The app's screen shows it.
    private(set) var standbyEndsAt: Date?
    /// Whether standby is currently holding the mic ready.
    private(set) var isStandbyUp = false

    var level: Float { recorder.level }
    var isRecording: Bool { phase == .recording }

    /// Whether a dictation can start at all, and why not when it cannot.
    ///
    /// Backed by stored properties rather than reading the Keychain and the
    /// permission store on each access: those live outside the observation graph, so
    /// a computed version leaves the view showing "add your API key" after the key
    /// has been saved, with no way for SwiftUI to know it should redraw. Call
    /// `refreshConfiguration()` whenever either could have changed.
    var blocker: String? {
        if !hasAPIKey { return "Add your Groq API key in Settings to start dictating." }
        if !hasMicPermission { return "WhimprFlow needs microphone access." }
        if let ringError { return ringError }
        return nil
    }

    private(set) var hasAPIKey = APIKey.isSet
    private(set) var hasMicPermission = Recorder.hasPermission

    /// The keys with their rate-limit clocks, kept across dictations so a limited
    /// key stays skipped for as long as Groq asked. Rebuilt when the list changes.
    private var ring: KeyRing?
    /// Why the ring could not be built, shown in place of dictating. This is the
    /// core refusing a request — in practice a linked library older than this Swift
    /// — and reporting it as "no API key" sent someone to re-enter a key that was fine.
    private var ringError: String?

    init() {
        buildRing()
    }

    /// Re-read the things the observation graph cannot see for itself.
    func refreshConfiguration() {
        hasAPIKey = APIKey.isSet
        hasMicPermission = Recorder.hasPermission
        buildRing()
    }

    private func buildRing() {
        do {
            ring = try KeyRing(keys: APIKey.loadAll())
            ringError = nil
        } catch {
            ring = nil
            ringError = error.localizedDescription
        }
    }

    // MARK: - Lifecycle

    /// Enter standby: hold the microphone open and publish the liveness heartbeat, so
    /// the keyboard's mic key can start a dictation without opening this app.
    ///
    /// The engine has to be genuinely *running*, not merely permitted to run.
    /// `UIBackgroundModes: audio` keeps an app alive only while audio is active;
    /// declaring the mode and idling gets the app suspended seconds after it
    /// backgrounds, which silently made every mic-key tap fall back to opening it.
    ///
    /// The heartbeat is a heartbeat and not a flag because the app can be killed
    /// without getting to clear a flag, and a stale "I'm alive" leaves the mic key
    /// doing nothing at all. Its stopping is how the keyboard learns to bounce.
    func startStandby() {
        guard settings.keepSessionAlive else { return }
        guard blocker == nil else { return }
        // A call or Siri arriving mid-sentence takes the microphone away. Transcribe
        // what was captured rather than discarding it: the words up to the
        // interruption are still the user's, and losing them silently is worse than
        // an ending that stops short of where they meant to finish.
        recorder.onCaptureInterrupted = { [weak self] in
            Task { @MainActor in await self?.finishRecording() }
        }
        do {
            try recorder.startEngine()
        } catch {
            // Standby is an optimization; failing it must not break dictation, which
            // still works by opening the app.
            stopStandby()
            return
        }
        isStandbyUp = true
        armIdleTimer()
        if heartbeat == nil {
            Handoff.markAlive()
            heartbeat = Task { [weak self] in
                while !Task.isCancelled {
                    try? await Task.sleep(for: .seconds(Handoff.livenessWindow / 3))
                    // Standby steps aside while a dictation holds the mic; both count.
                    guard let self, self.recorder.isAlive else { return }
                    Handoff.markAlive()
                }
            }
        }
        if phase != .recording, phase != .transcribing {
            activity.ensure(activityState(.ready))
        }
    }

    /// Wind the idle clock back to the full timeout, or clear it when there is none.
    ///
    /// Called from every event that means the user is still here: standby starting,
    /// a dictation ending however it ended, the app coming to the foreground. Not
    /// from a dictation *starting* — the clock stops for the duration, since a long
    /// dictation is the opposite of idle.
    private func armIdleTimer() {
        idleTimer?.cancel()
        idleTimer = nil
        guard isStandbyUp, let seconds = settings.standbyTimeout.seconds else {
            standbyEndsAt = nil
            return
        }
        let endsAt = Date().addingTimeInterval(seconds)
        standbyEndsAt = endsAt
        idleTimer = Task { [weak self] in
            try? await Task.sleep(for: .seconds(seconds))
            guard !Task.isCancelled, let self else { return }
            // Never mid-dictation: the clock was stopped, but a stop that raced the
            // sleep would otherwise drop the recording.
            guard self.phase != .recording, self.phase != .transcribing else { return }
            self.stopStandby()
        }
    }

    private func pauseIdleTimer() {
        idleTimer?.cancel()
        idleTimer = nil
        standbyEndsAt = nil
    }

    /// The microphone's name for people. iOS names the built-in one by its port type
    /// ("MicrophoneBuiltIn" on the simulator, "iPhone Microphone" on a phone), and an
    /// accessory by what its maker called it — which is the useful case, so that is
    /// passed through and only the built-in one is spelled out.
    private static func describe(_ port: AVAudioSessionPortDescription?) -> String? {
        guard let port else { return nil }
        switch port.portType {
        case .builtInMic:
            return UIDevice.current.userInterfaceIdiom == .pad ? "iPad Microphone" : "iPhone Microphone"
        default:
            return port.portName
        }
    }

    /// What the Live Activity should say for a phase, from the state this type
    /// already holds.
    private func activityState(
        _ phase: StandbyActivityAttributes.ContentState.Phase
    ) -> StandbyActivityAttributes.ContentState {
        StandbyActivityAttributes.ContentState(
            phase: phase,
            inputName: Handoff.inputName,
            startedAt: phase == .listening ? Handoff.captureStartedAt : nil,
            releaseAt: phase == .ready ? standbyEndsAt : nil
        )
    }

    /// The Live Activity cannot be requested from the background. The app calls this
    /// on every foreground so a session that started while it could not be shown —
    /// or that iOS ended at its eight-hour limit — gets its glyph back.
    func resumeActivityIfNeeded() {
        activity.resumeIfNeeded()
    }

    /// Leave standby: release the microphone and stop claiming to be reachable.
    func stopStandby() {
        heartbeat?.cancel()
        heartbeat = nil
        pauseIdleTimer()
        isStandbyUp = false
        activity.end()
        Handoff.clearAlive()
        // Never while a dictation is in flight — that would drop the recording.
        if !recorder.isCapturing {
            recorder.stopEngine()
        }
    }

    /// Re-read the standby preference and act on it.
    func applyStandbyPreference() {
        if settings.keepSessionAlive {
            startStandby()
            // A changed timeout takes effect now, not after the next dictation.
            armIdleTimer()
            if isStandbyUp, phase != .recording, phase != .transcribing {
                activity.ensure(activityState(.ready))
            }
        } else {
            stopStandby()
        }
    }

    /// Listen for the keyboard's mic key.
    ///
    /// Darwin notifications are the only cross-process wake-up an extension can send,
    /// and they carry no payload — so these mean exactly "start" and "stop", and
    /// everything else is read from the shared container.
    func observeKeyboard() {
        let observer = Unmanaged.passUnretained(self).toOpaque()
        Handoff.observe(.start, observer: observer) { _, observer, _, _, _ in
            guard let observer else { return }
            let controller = Unmanaged<DictationController>
                .fromOpaque(observer).takeUnretainedValue()
            Task { @MainActor in controller.startRecording() }
        }
        Handoff.observe(.stop, observer: observer) { _, observer, _, _, _ in
            guard let observer else { return }
            let controller = Unmanaged<DictationController>
                .fromOpaque(observer).takeUnretainedValue()
            Task { @MainActor in await controller.finishRecording() }
        }
        Handoff.observe(.cancel, observer: observer) { _, observer, _, _, _ in
            guard let observer else { return }
            let controller = Unmanaged<DictationController>
                .fromOpaque(observer).takeUnretainedValue()
            Task { @MainActor in controller.cancelRecording() }
        }
        // The keyboard's pill changed the level in the container. `Settings` caches
        // it, so re-read or the next dictation is cleaned at the old level.
        Handoff.observe(.settings, observer: observer) { _, observer, _, _, _ in
            guard let observer else { return }
            let controller = Unmanaged<DictationController>
                .fromOpaque(observer).takeUnretainedValue()
            Task { @MainActor in controller.settings.reloadLevel() }
        }
        // "Release the mic", from the keyboard's menu or the island. The next
        // foreground visit re-arms standby; that is the designed round trip.
        Handoff.observe(.release, observer: observer) { _, observer, _, _, _ in
            guard let observer else { return }
            let controller = Unmanaged<DictationController>
                .fromOpaque(observer).takeUnretainedValue()
            Task { @MainActor in controller.stopStandby() }
        }
    }

    // MARK: - The loop

    func toggle() {
        switch phase {
        case .recording: Task { await finishRecording() }
        default: startRecording()
        }
    }

    func startRecording() {
        guard blocker == nil else {
            phase = .failed(blocker ?? "not configured")
            return
        }
        guard phase != .recording, phase != .transcribing else { return }
        do {
            try recorder.beginCapture()
            if settings.soundOnStart {
                // The same pop the Mac plays, and the mic looks away while it sounds.
                Chime.shared.playStart()
                recorder.muteInput(for: Chime.duration)
            }
            pauseIdleTimer()
            // The keyboard and the island show which mic and for how long; neither
            // can see this process's audio session, so it is written down here.
            Handoff.inputName = Self.describe(AVAudioSession.sharedInstance().currentRoute.inputs.first)
            Handoff.captureStartedAt = Date()
            phase = .recording
            Handoff.state = .recording
            if isStandbyUp { activity.ensure(activityState(.listening)) }
        } catch {
            fail(error.localizedDescription)
        }
    }

    func cancelRecording() {
        guard phase == .recording else { return }
        _ = recorder.endCapture()
        phase = .idle
        settle()
    }

    /// Back to ready after a dictation ended, however it ended: the idle clock is
    /// wound back, the island says "ready", the keyboard sees idle.
    private func settle() {
        Handoff.captureStartedAt = nil
        Handoff.state = .idle
        armIdleTimer()
        if isStandbyUp { activity.ensure(activityState(.ready)) }
    }

    func finishRecording() async {
        guard phase == .recording else { return }
        let samples = recorder.endCapture()
        phase = .transcribing
        Handoff.state = .transcribing
        Handoff.captureStartedAt = nil
        if isStandbyUp { activity.ensure(activityState(.transcribing)) }

        // A tap rather than a dictation. Nothing to send, and an empty request would
        // come back as a hallucinated sentence rather than an empty string.
        guard samples.count > Int(Recorder.sampleRate) / 4 else {
            phase = .idle
            settle()
            return
        }

        guard let ring, ring.count > 0 else {
            fail(ringError ?? "no API key is set")
            return
        }
        let client = GroqClient(ring: ring)
        let dictionary = settings.dictionary
        let level = settings.level

        do {
            let transcript = try await recognize(samples: samples, client: client, dictionary: dictionary)
            guard !transcript.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
                phase = .idle
                settle()
                return
            }
            let finished = try await clean(
                transcript: transcript, client: client, dictionary: dictionary, level: level
            )
            publish(finished)
        } catch {
            fail(error.localizedDescription)
        }
    }

    // MARK: - Recognition

    /// Two passes, and only when the dictionary matched.
    ///
    /// Prompting biases every dictation, including the overwhelming majority with no
    /// dictionary word in them — and a prompt Whisper cannot hear in the audio is one
    /// it may emit anyway. Running unprompted first means the common case is
    /// untouched and the biased result has an unbiased one to be checked against.
    /// Without that comparison a hallucinated name goes straight to the cursor.
    private func recognize(
        samples: [Float], client: GroqClient, dictionary: DictionaryStore
    ) async throws -> String {
        let wav = Recorder.wav(from: samples)
        let unprompted = try await client.transcribe(wav: wav, prompt: nil)

        let bias = try WhimprCore.asrBiasPrompt(unprompted: unprompted, dictionary: dictionary)
        // No match means one pass was the whole job. Do not prompt anyway.
        guard let prompt = bias.prompt else { return unprompted }

        guard let prompted = try? await client.transcribe(wav: wav, prompt: prompt) else {
            return unprompted // a failed second pass is not a failed dictation
        }
        let accept = try WhimprCore.asrAcceptPrompted(
            unprompted: unprompted, prompted: prompted, vocab: bias.vocab
        )
        return accept ? prompted : unprompted
    }

    // MARK: - Cleanup

    /// Cleanup, with raw as the fallback rather than as a failure.
    ///
    /// Every path here ends in text worth inserting: a cloud error, a truncated
    /// reply, or a gate rejection all produce the raw transcript with the reason
    /// recorded. An untidy-but-faithful paste beats a wrong-but-clean one, and beats
    /// nothing at all by a wider margin still.
    private func clean(
        transcript: String, client: GroqClient, dictionary: DictionaryStore, level: CleanupLevel
    ) async throws -> Finished {
        let prepared = try WhimprCore.prepare(
            raw: transcript, level: level, dictionary: dictionary
        )
        guard level != .none else {
            return try WhimprCore.rawOnly(
                prepared: prepared, degraded: nil, dictionary: dictionary, rawMode: true
            )
        }
        do {
            let reply = try await client.cleanup(prepared: prepared)
            return try WhimprCore.finish(
                prepared: prepared, modelOutput: reply, engine: .cloud, dictionary: dictionary
            )
        } catch {
            // No local model to fall back to on this platform, so raw is the floor.
            return try WhimprCore.rawOnly(
                prepared: prepared,
                degraded: "cloud_error: \(error.localizedDescription)",
                dictionary: dictionary
            )
        }
    }

    // MARK: - Output

    private func publish(_ finished: Finished) {
        lastEngine = finished.engine
        lastDegraded = finished.degraded
        Handoff.publish(finished)
        // Also on the pasteboard, so a dictation is usable even when the keyboard is
        // not installed or Full Access is off.
        UIPasteboard.general.string = finished.text
        phase = .done(finished.text)
        settle()
    }

    private func fail(_ message: String) {
        phase = .failed(message)
        Handoff.captureStartedAt = nil
        Handoff.state = .failed
        armIdleTimer()
        if isStandbyUp { activity.ensure(activityState(.ready)) }
    }

    func clearTerminalPhase() {
        switch phase {
        case .done, .failed: phase = .idle
        default: break
        }
    }
}
