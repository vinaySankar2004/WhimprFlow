import Foundation

/// Swift's side of the C ABI in `crates/whimpr-ffi`.
///
/// Everything that decides *what text gets inserted* lives on the other side of this
/// boundary: the prompt, the levels, the gates, the dictionary, and — importantly —
/// the order they run in. This file is a transport, and deliberately contains no
/// judgement of its own. If you find yourself wanting to post-process a string here,
/// the change belongs in `whimpr-core` where macOS gets it too.
enum WhimprCore {
    enum Failure: LocalizedError {
        /// The core returned `{"status":"error"}` — a malformed request, or a panic
        /// it caught on our behalf.
        case core(String)
        /// The bridge itself misbehaved. Should be unreachable; if it fires, the
        /// linked `.a` and this file are out of step.
        case bridge(String)

        var errorDescription: String? {
            switch self {
            case let .core(m): return "whimpr-core: \(m)"
            case let .bridge(m): return "whimpr bridge: \(m)"
            }
        }
    }

    // MARK: - Transport

    /// Send one JSON request and return its `result`.
    ///
    /// The C string that comes back is always ours to release, including on the error
    /// paths — hence the `defer` immediately after the null check, before anything
    /// that can throw.
    private static func call(_ request: [String: Any]) throws -> Any {
        let requestData = try JSONSerialization.data(withJSONObject: request)
        guard let requestText = String(data: requestData, encoding: .utf8) else {
            throw Failure.bridge("the request was not representable as UTF-8")
        }

        guard let out = requestText.withCString({ whimpr_call($0) }) else {
            // Documented never to happen: the bridge returns an error *response*
            // rather than null, precisely so Swift has something to show a user.
            throw Failure.bridge("the core returned no response")
        }
        defer { whimpr_string_free(out) }

        let responseText = String(cString: out)
        guard let data = responseText.data(using: .utf8),
              let object = try? JSONSerialization.jsonObject(with: data),
              let response = object as? [String: Any],
              let status = response["status"] as? String
        else {
            throw Failure.bridge("unparseable response: \(responseText.prefix(200))")
        }

        if status == "error" {
            throw Failure.core(response["message"] as? String ?? "unknown")
        }
        guard let result = response["result"] else {
            throw Failure.bridge("an ok response carried no result")
        }
        return result
    }

    private static func callObject(_ request: [String: Any]) throws -> [String: Any] {
        guard let dict = try call(request) as? [String: Any] else {
            throw Failure.bridge("expected an object for op \(request["op"] ?? "?")")
        }
        return dict
    }

    // MARK: - Ops

    /// The bridge version the linked library was built with, for a startup check that
    /// the `.a` matches this file. A stale library is otherwise silent: the ops it
    /// does support keep working and the new one returns "unknown op".
    static func bridgeVersion() throws -> Int {
        try callObject(["op": "version"])["bridge"] as? Int ?? 0
    }

    /// Everything the cleanup provider needs, computed from a raw transcript.
    static func prepare(
        raw: String,
        level: CleanupLevel,
        dictionary: DictionaryStore,
        appBundleID: String? = nil
    ) throws -> Prepared {
        var request: [String: Any] = [
            "op": "prepare",
            "raw": raw,
            "level": level.rawValue,
            "dictionary": dictionary.payload,
        ]
        if let appBundleID { request["app_bundle_id"] = appBundleID }
        return try Prepared(payload: callObject(request))
    }

    /// The deterministic passes, the gate, and the trailing dictionary and register
    /// passes over the provider's reply. This is the only way to turn model output
    /// into text worth inserting — there is no correct way to assemble it here.
    static func finish(
        prepared: Prepared,
        modelOutput: String,
        engine: Engine,
        dictionary: DictionaryStore,
        rawMode: Bool = false
    ) throws -> Finished {
        try Finished(payload: callObject([
            "op": "finish",
            "prepared": prepared.payload,
            "model_output": modelOutput,
            "engine": engine.rawValue,
            "dictionary": dictionary.payload,
            "raw_mode": rawMode,
        ]))
    }

    /// The raw path: cleanup is off by request, or every engine failed. Still applies
    /// the dictionary and the register, which are settings rather than cleanup.
    static func rawOnly(
        prepared: Prepared,
        degraded: String?,
        dictionary: DictionaryStore,
        rawMode: Bool = false
    ) throws -> Finished {
        var request: [String: Any] = [
            "op": "raw_only",
            "prepared": prepared.payload,
            "dictionary": dictionary.payload,
            "raw_mode": rawMode,
        ]
        if let degraded { request["degraded"] = degraded }
        return try Finished(payload: callObject(request))
    }

    /// First half of the two-pass ASR bias.
    ///
    /// A nil `prompt` means the dictionary matched nothing in this utterance and the
    /// caller **must not** run a second pass. Priming Whisper unconditionally makes
    /// it emit words it never heard, and the unprompted transcript is the only thing
    /// that catches it.
    static func asrBiasPrompt(unprompted: String, dictionary: DictionaryStore) throws -> BiasPrompt {
        let result = try callObject([
            "op": "asr_bias_prompt",
            "unprompted": unprompted,
            "dictionary": dictionary.payload,
        ])
        return BiasPrompt(
            vocab: result["vocab"] as? [[String: Any]] ?? [],
            prompt: result["prompt"] as? String
        )
    }

    /// Second half: keep the prompted transcript, or fall back to the unprompted one.
    static func asrAcceptPrompted(
        unprompted: String,
        prompted: String,
        vocab: [[String: Any]]
    ) throws -> Bool {
        guard let accepted = try call([
            "op": "asr_accept_prompted",
            "unprompted": unprompted,
            "prompted": prompted,
            "vocab": vocab,
        ]) as? Bool else {
            throw Failure.bridge("asr_accept_prompted did not return a bool")
        }
        return accepted
    }
}

// MARK: - Types

/// How aggressively cleanup may edit, and in what register. Raw values match the
/// core's serde representation; adding one here without adding it there produces a
/// parse error from the bridge rather than silent misbehaviour.
enum CleanupLevel: String, Codable, CaseIterable {
    /// Paste exactly what was said. No model call.
    case none
    /// Casual chat register: same edits as `light`, all lowercase.
    case messaging
    /// Remove fillers and fix grammar only. The default.
    case light
}

/// Which engine produced the text.
enum Engine: String, Codable {
    case cloud, local, raw
}

/// The output of `prepare`, carried back to `finish` untouched.
///
/// Deliberately opaque. Swift reads only the two fields it must — what to POST, and
/// how many tokens to allow — and treats the rest as the core's business. A field
/// added on the Rust side therefore needs no change here and cannot be dropped in
/// transit, which matters because one of them is the vocabulary the gate uses to tell
/// a dictionary correction apart from a hallucination.
struct Prepared {
    let payload: [String: Any]

    /// The chat turns to send verbatim, in order.
    let messages: [ChatMessage]
    /// Token budget, scaled to the input. A fixed budget silently truncates the paste.
    let maxTokens: Int

    init(payload: [String: Any]) throws {
        self.payload = payload
        guard let rawMessages = payload["messages"] as? [[String: Any]] else {
            throw WhimprCore.Failure.bridge("prepared carried no messages")
        }
        messages = rawMessages.compactMap { message in
            guard let role = message["role"] as? String,
                  let content = message["content"] as? String else { return nil }
            return ChatMessage(role: role, content: content)
        }
        guard messages.count == rawMessages.count else {
            throw WhimprCore.Failure.bridge("a message was missing role or content")
        }
        guard let tokens = payload["max_tokens"] as? Int else {
            throw WhimprCore.Failure.bridge("prepared carried no max_tokens")
        }
        maxTokens = tokens
    }

    /// The transcript to fall back to if the provider fails outright — already has
    /// spoken layout cues turned into real breaks.
    var rawFallback: String { payload["raw_fallback"] as? String ?? "" }
}

/// One chat turn, in the chat-completions shape both providers speak.
struct ChatMessage {
    let role: String
    let content: String

    var wire: [String: String] { ["role": role, "content": content] }
}

/// The text to insert, plus which engine produced it and why that was not the
/// selected one.
///
/// `degraded` is not decoration: every fallback in this app is deliberately silent,
/// so a run of raw or slow insertions has no explanation unless the reason was
/// recorded as it happened.
struct Finished {
    let text: String
    let engine: Engine
    let degraded: String?

    init(payload: [String: Any]) throws {
        guard let text = payload["text"] as? String else {
            throw WhimprCore.Failure.bridge("finished carried no text")
        }
        self.text = text
        engine = Engine(rawValue: payload["engine"] as? String ?? "") ?? .raw
        degraded = payload["degraded"] as? String
    }
}

/// What `asrBiasPrompt` answers.
struct BiasPrompt {
    /// The entries the prefilter selected. Hand these back to `asrAcceptPrompted`,
    /// which must judge against the same list.
    let vocab: [[String: Any]]
    /// `initial_prompt` for the second pass, or nil for "do not run one".
    let prompt: String?
}

/// The user's dictionary, held in the shape the core persists so it crosses the
/// bridge unchanged.
struct DictionaryStore {
    private(set) var entries: [[String: Any]]

    init(entries: [[String: Any]] = []) {
        self.entries = entries
    }

    var payload: [String: Any] { ["entries": entries] }

    var isEmpty: Bool { entries.isEmpty }
}
