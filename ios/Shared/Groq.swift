import Foundation

/// The Groq client: hosted Whisper for recognition, chat-completions for cleanup.
///
/// The wire format mirrors `crates/whimpr-cleanup/src/lib.rs` deliberately — same
/// temperature, same scaled token budget, same `reasoning_effort` handling — because
/// a difference here is a difference in what gets pasted, and the whole point of
/// linking the core was that the two platforms behave alike.
struct GroqClient {
    let apiKey: String
    var session: URLSession = .shared

    enum Failure: LocalizedError {
        case http(Int, String)
        case empty
        /// The model stopped because it ran out of budget. A complete raw transcript
        /// beats a clean half of one, so this is an error and not a result.
        case truncated
        case notConfigured

        /// Written for whoever is holding the phone, not for whoever wrote this.
        ///
        /// The raw body is kept only for statuses with no established meaning: an
        /// unexplained 502 is worth showing verbatim, whereas answering a rejected
        /// key with a JSON blob tells the one person who can fix it nothing about
        /// how.
        var errorDescription: String? {
            switch self {
            case let .http(code, body):
                switch code {
                case 401:
                    return "Groq rejected the API key. Check it in Settings."
                // Not the same as 401, and must not be reported as one: the key is
                // accepted and the *request* is refused.
                //
                // In practice, on this app, it has meant a VPN every time. Groq sits
                // behind a CDN that refuses datacenter exit addresses, so a perfectly
                // good key fails from behind a VPN and works the moment it is off.
                // Reported as "check your key" — which is what 401 and 403 collapsed
                // to before — it sends people to delete and re-create a key that was
                // never the problem, which is exactly what happened here.
                case 403:
                    return "Groq refused the request (403). This is usually a VPN — "
                        + "Groq blocks VPN and datacenter addresses, so try turning it "
                        + "off. \(Self.detail(body))"
                case 429:
                    return "Groq is rate limiting — the daily free quota may be spent. Try again later."
                case 500...599:
                    return "Groq is having trouble (error \(code)). Try again in a moment."
                default:
                    return "Groq returned \(code). \(Self.detail(body))"
                }
            case .empty: return "Groq returned no text."
            case .truncated: return "The reply was cut off before it finished."
            case .notConfigured: return "No API key is set."
            }
        }

        /// Pull the human-readable sentence out of an error body.
        ///
        /// Groq answers `{"error":{"message":"…"}}`. Showing that beats any wording
        /// invented here, because the cause of a refusal is on their side and only
        /// they know which one it was. Falls back to the raw body, trimmed, when the
        /// shape is not what is expected — an unparsed body still says more than a
        /// status code alone.
        static func detail(_ body: String) -> String {
            guard let data = body.data(using: .utf8),
                  let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
                  let error = object["error"] as? [String: Any],
                  let message = error["message"] as? String
            else {
                let trimmed = body.trimmingCharacters(in: .whitespacesAndNewlines)
                return trimmed.isEmpty ? "No detail was returned." : String(trimmed.prefix(200))
            }
            return message
        }
    }

    // MARK: - Recognition

    /// Transcribe 16 kHz mono PCM.
    ///
    /// `prompt` is Whisper's `initial_prompt`, and `language` is pinned rather than
    /// detected: auto-detection on a short push-to-talk clip does not merely
    /// mis-spell a word when it guesses wrong, it *translates* the utterance.
    func transcribe(wav: Data, prompt: String?) async throws -> String {
        var request = URLRequest(url: URL(string: Settings.Groq.transcriptions)!)
        request.httpMethod = "POST"
        request.setValue("Bearer \(apiKey)", forHTTPHeaderField: "Authorization")

        let boundary = "whimpr.\(UUID().uuidString)"
        request.setValue("multipart/form-data; boundary=\(boundary)", forHTTPHeaderField: "Content-Type")

        var body = Data()
        func field(_ name: String, _ value: String) {
            body.append("--\(boundary)\r\n")
            body.append("Content-Disposition: form-data; name=\"\(name)\"\r\n\r\n")
            body.append("\(value)\r\n")
        }
        body.append("--\(boundary)\r\n")
        body.append("Content-Disposition: form-data; name=\"file\"; filename=\"audio.wav\"\r\n")
        body.append("Content-Type: audio/wav\r\n\r\n")
        body.append(wav)
        body.append("\r\n")
        field("model", Settings.Groq.asrModel)
        field("response_format", "json")
        field("language", "en")
        // Dropping this is how the dictionary silently stops working on cloud ASR.
        if let prompt, !prompt.isEmpty { field("prompt", prompt) }
        body.append("--\(boundary)--\r\n")
        request.httpBody = body

        let json = try await send(request)
        guard let text = json["text"] as? String else { throw Failure.empty }
        return text.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    // MARK: - Cleanup

    /// Run cleanup over the messages `whimpr-core` built. The messages and the token
    /// budget both come from `Prepared` — nothing about the prompt is decided here.
    func cleanup(prepared: Prepared) async throws -> String {
        var body: [String: Any] = [
            "model": Settings.Groq.cleanupModel,
            // Greedy, matching the Mac and the local worker: cleanup is a mechanical
            // rewrite with one right answer, so sampling buys nothing and costs
            // repeatability.
            "temperature": 0,
            // Scaled to the dictation, never fixed. A fixed ceiling does not fail, it
            // truncates the paste mid-sentence — and the gates do not catch losing
            // the last tenth of a message.
            "max_tokens": prepared.maxTokens,
            "messages": prepared.messages.map(\.wire),
        ]
        // Reasoning models think before answering, and on Groq those hidden tokens
        // come out of the same budget and the same wall clock the user is waiting on.
        // Cleanup is not a puzzle; buy none of it.
        let askedForLowReasoning = Self.takesReasoningEffort(Settings.Groq.cleanupModel)
            && !Self.reasoningEffortRefused
        if askedForLowReasoning { body["reasoning_effort"] = "low" }

        do {
            return try await postChat(body)
        } catch let Failure.http(code, _) where code == 400 && askedForLowReasoning {
            // A 400 is how an endpoint says it does not know a parameter. Dropping
            // the optimization beats losing cleanup entirely — there is no local
            // model here to fall back to, so the alternative is raw pastes forever.
            Self.reasoningEffortRefused = true
            body.removeValue(forKey: "reasoning_effort")
            return try await postChat(body)
        }
    }

    private func postChat(_ body: [String: Any]) async throws -> String {
        var request = URLRequest(url: URL(string: Settings.Groq.chatCompletions)!)
        request.httpMethod = "POST"
        request.setValue("Bearer \(apiKey)", forHTTPHeaderField: "Authorization")
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.httpBody = try JSONSerialization.data(withJSONObject: body)

        let json = try await send(request)
        guard let choices = json["choices"] as? [[String: Any]], let first = choices.first else {
            throw Failure.empty
        }
        // Checked before the text is used: a truncated cleanup is a plausible-looking
        // message with the end missing, which no gate will reject.
        if let reason = first["finish_reason"] as? String, reason == "length" {
            throw Failure.truncated
        }
        guard let message = first["message"] as? [String: Any],
              let content = message["content"] as? String,
              !content.trimmingCharacters(in: .whitespaces).isEmpty
        else { throw Failure.empty }
        return content
    }

    // MARK: - Transport

    private func send(_ request: URLRequest) async throws -> [String: Any] {
        let (data, response) = try await session.data(for: request)
        let code = (response as? HTTPURLResponse)?.statusCode ?? 0
        guard (200..<300).contains(code) else {
            throw Failure.http(code, String(data: data, encoding: .utf8) ?? "")
        }
        guard let json = try JSONSerialization.jsonObject(with: data) as? [String: Any] else {
            throw Failure.empty
        }
        return json
    }

    /// Remembered for the process, so a rejected parameter costs one wasted call
    /// rather than one per dictation.
    nonisolated(unsafe) private static var reasoningEffortRefused = false

    private static func takesReasoningEffort(_ model: String) -> Bool {
        model.contains("gpt-oss") || model.hasPrefix("o1") || model.hasPrefix("o3")
    }
}

private extension Data {
    mutating func append(_ string: String) {
        append(Data(string.utf8))
    }
}
