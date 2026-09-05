//! The C ABI over `whimpr-core`, for shells that are not Rust.
//!
//! # Why one function and not thirty
//!
//! Every value this bridge carries — [`Prepared`], [`Finished`], `CleanupContext`,
//! `DictionaryStore` — already derives `Serialize`/`Deserialize`, because the core
//! persists them. So the entire surface is one entry point taking a JSON request and
//! returning a JSON response, rather than a hand-written struct ABI with a `repr(C)`
//! mirror of each type, a constructor and a destructor for each, and a way for the
//! two definitions to drift the first time a field is added.
//!
//! The cost is a serialize/deserialize per call. On a dictation that is about to
//! spend seconds in an HTTP round trip, it does not register.
//!
//! # Why the ops are coarse
//!
//! [`Op::Prepare`] and [`Op::Finish`] each do several things that a caller could in
//! principle do separately. That is deliberate: the order of those steps is
//! load-bearing in ways that fail *silently* (see `whimpr_core::pipeline`), so the
//! bridge does not expose a way to get it wrong. Swift decides *what* to send to the
//! provider and *when*; it never decides what runs before or after.
//!
//! # Contract
//!
//! ```c
//! char *out = whimpr_call("{\"op\":\"version\"}");
//! // ... read it ...
//! whimpr_string_free(out);
//! ```
//!
//! - The argument must be a NUL-terminated UTF-8 C string. `NULL` is answered with an
//!   error response, not a crash.
//! - The return is always a non-NULL NUL-terminated JSON C string, and always the
//!   caller's to free with [`whimpr_string_free`]. Freeing it any other way (Swift's
//!   `free`, say) is undefined: it was allocated by Rust's allocator.
//! - Every response is `{"status":"ok","result":…}` or `{"status":"error","message":…}`.
//!   A panic inside the core becomes the latter — unwinding across an FFI boundary is
//!   undefined behaviour, so [`std::panic::catch_unwind`] stops it here.

use std::ffi::{c_char, CStr, CString};

use serde::{Deserialize, Serialize};
use whimpr_core::asr::prompt::{accept_prompted, build_initial_prompt};
use whimpr_core::cleanup::{CleanupLevel, VocabEntry};
use whimpr_core::dictionary::DictionaryStore;
use whimpr_core::pipeline::{self, Engine, Prepared};

/// A request. `op` selects the variant; unknown ops are an error response, so a Swift
/// build newer than the linked core degrades to a message rather than a crash.
#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum Op {
    /// Bridge version, for a startup sanity check that the linked `.a` is the one the
    /// Swift was written against.
    Version,

    /// Everything the provider needs, computed from a raw transcript.
    Prepare {
        raw: String,
        level: CleanupLevel,
        #[serde(default)]
        dictionary: DictionaryStore,
        #[serde(default)]
        app_bundle_id: Option<String>,
    },

    /// The deterministic passes, the gate, and the trailing dictionary/register
    /// passes over a provider's output.
    Finish {
        prepared: Prepared,
        model_output: String,
        engine: Engine,
        #[serde(default)]
        dictionary: DictionaryStore,
        #[serde(default)]
        raw_mode: bool,
    },

    /// The raw path: cleanup off by request, or every engine unavailable.
    RawOnly {
        prepared: Prepared,
        #[serde(default)]
        degraded: Option<String>,
        #[serde(default)]
        dictionary: DictionaryStore,
        #[serde(default)]
        raw_mode: bool,
    },

    /// The first half of the two-pass ASR bias: given an unprompted transcript,
    /// which vocabulary is relevant and what should the second pass be primed with.
    ///
    /// A `prompt` of `null` means the prefilter matched nothing, and the caller must
    /// **not** run a second pass — one pass was the whole job.
    AsrBiasPrompt {
        unprompted: String,
        #[serde(default)]
        dictionary: DictionaryStore,
    },

    /// The second half: is the prompted transcript a dictionary correction, or did
    /// priming make Whisper emit words it did not hear? `false` means keep the
    /// unprompted one.
    AsrAcceptPrompted {
        unprompted: String,
        prompted: String,
        #[serde(default)]
        vocab: Vec<VocabEntry>,
    },
}

/// What [`Op::AsrBiasPrompt`] answers.
#[derive(Debug, Serialize)]
struct BiasPrompt {
    /// The entries the prefilter selected — pass these back to
    /// [`Op::AsrAcceptPrompted`], which needs the same list.
    vocab: Vec<VocabEntry>,
    /// `initial_prompt` for the second pass, or `null` for "do not run one".
    prompt: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum Response {
    Ok { result: serde_json::Value },
    Error { message: String },
}

/// The bridge's version. Bumped when a request or response shape changes in a way
/// Swift must notice; not tied to the crate version, which moves for other reasons.
const BRIDGE_VERSION: u32 = 1;

fn dispatch(op: Op) -> Result<serde_json::Value, String> {
    let value = match op {
        Op::Version => serde_json::json!({
            "bridge": BRIDGE_VERSION,
            "crate": env!("CARGO_PKG_VERSION"),
        }),

        Op::Prepare {
            raw,
            level,
            dictionary,
            app_bundle_id,
        } => to_value(&pipeline::prepare(&raw, level, &dictionary, app_bundle_id))?,

        Op::Finish {
            prepared,
            model_output,
            engine,
            dictionary,
            raw_mode,
        } => to_value(&pipeline::finish(
            &prepared,
            &model_output,
            engine,
            &dictionary,
            raw_mode,
        ))?,

        Op::RawOnly {
            prepared,
            degraded,
            dictionary,
            raw_mode,
        } => to_value(&pipeline::raw_only(
            &prepared, degraded, &dictionary, raw_mode,
        ))?,

        Op::AsrBiasPrompt {
            unprompted,
            dictionary,
        } => {
            // The same 15-entry cap the cleanup prompt uses, against the same text.
            let vocab = dictionary.prefilter(&unprompted, 15);
            let prompt = build_initial_prompt(&vocab);
            to_value(&BiasPrompt { vocab, prompt })?
        }

        Op::AsrAcceptPrompted {
            unprompted,
            prompted,
            vocab,
        } => serde_json::json!(accept_prompted(&unprompted, &prompted, &vocab)),
    };
    Ok(value)
}

fn to_value<T: Serialize>(v: &T) -> Result<serde_json::Value, String> {
    serde_json::to_value(v).map_err(|e| format!("serializing the response failed: {e}"))
}

/// The whole bridge, minus the pointer handling.
///
/// Public so `tests/parity.rs` can drive the exact path iOS takes — request parsing,
/// dispatch and response serialization included — and compare it against calling
/// `whimpr_core::pipeline` directly, which is the path macOS takes. Everything the
/// pointer layer adds is memory management, and that is tested separately.
pub fn handle(request: &str) -> String {
    let response = match serde_json::from_str::<Op>(request) {
        Ok(op) => match dispatch(op) {
            Ok(result) => Response::Ok { result },
            Err(message) => Response::Error { message },
        },
        Err(e) => Response::Error {
            message: format!("could not parse the request: {e}"),
        },
    };
    // A response that will not serialize is a bug in this crate, not the caller's
    // problem — but it still must not return NULL, so it degrades to a fixed string.
    serde_json::to_string(&response).unwrap_or_else(|_| {
        r#"{"status":"error","message":"the response could not be serialized"}"#.to_string()
    })
}

/// Call the bridge. See the module docs for the contract.
///
/// # Safety
///
/// `request` must be either NULL or a valid pointer to a NUL-terminated C string that
/// stays valid for the duration of the call. The returned pointer must be released
/// with [`whimpr_string_free`] and by nothing else.
#[no_mangle]
pub unsafe extern "C" fn whimpr_call(request: *const c_char) -> *mut c_char {
    // Every failure below produces a JSON error string rather than a null or a
    // panic: the caller is Swift, which cannot catch either.
    let body = std::panic::catch_unwind(|| {
        if request.is_null() {
            return handle_error("the request pointer was NULL");
        }
        match CStr::from_ptr(request).to_str() {
            Ok(s) => handle(s),
            Err(e) => handle_error(&format!("the request was not valid UTF-8: {e}")),
        }
    })
    .unwrap_or_else(|_| handle_error("the core panicked"));

    // An interior NUL cannot occur — `body` is JSON from serde — but if it somehow
    // did, returning null would crash the caller, so fall back to a fixed message.
    CString::new(body)
        .unwrap_or_else(|_| {
            CString::new(r#"{"status":"error","message":"the response contained a NUL"}"#).unwrap()
        })
        .into_raw()
}

fn handle_error(message: &str) -> String {
    serde_json::to_string(&Response::Error {
        message: message.to_string(),
    })
    .unwrap_or_else(|_| r#"{"status":"error","message":"unknown"}"#.to_string())
}

/// Release a string returned by [`whimpr_call`].
///
/// # Safety
///
/// `ptr` must be NULL, or a pointer returned by [`whimpr_call`] that has not already
/// been passed here. Passing anything else — including a pointer Swift allocated — is
/// undefined behaviour.
#[no_mangle]
pub unsafe extern "C" fn whimpr_string_free(ptr: *mut c_char) {
    if !ptr.is_null() {
        drop(CString::from_raw(ptr));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use whimpr_core::dictionary::DictSource;

    fn call(request: serde_json::Value) -> serde_json::Value {
        let raw = handle(&request.to_string());
        serde_json::from_str(&raw).expect("the bridge returned invalid JSON")
    }

    fn ok(request: serde_json::Value) -> serde_json::Value {
        let v = call(request);
        assert_eq!(v["status"], "ok", "bridge returned an error: {v}");
        v["result"].clone()
    }

    fn dict() -> serde_json::Value {
        let mut d = DictionaryStore::default();
        d.add("Manvi", vec!["monvi".into()], DictSource::Manual);
        serde_json::to_value(d).unwrap()
    }

    #[test]
    fn version_reports_the_bridge_number() {
        assert_eq!(ok(serde_json::json!({"op": "version"}))["bridge"], 1);
    }

    /// The whole point of the bridge: Swift sends a transcript, gets the messages to
    /// POST, sends the reply back, and gets the text to insert — without ever
    /// deciding what runs in what order.
    #[test]
    fn a_full_round_trip_produces_pasteable_text() {
        let prepared = ok(serde_json::json!({
            "op": "prepare",
            "raw": "so um thanks monvi for the help today",
            "level": "messaging",
            "dictionary": dict(),
        }));
        // Swift POSTs these verbatim.
        let messages = prepared["messages"].as_array().expect("no messages");
        assert_eq!(messages[0]["role"], "system");
        assert!(prepared["max_tokens"].as_u64().unwrap() > 0);

        let finished = ok(serde_json::json!({
            "op": "finish",
            "prepared": prepared,
            "model_output": "Thanks Monvi for the help today.",
            "engine": "cloud",
            "dictionary": dict(),
        }));
        let text = finished["text"].as_str().unwrap();
        assert!(text.contains("manvi"), "dictionary + register did not run: {text:?}");
        assert!(!text.contains("Manvi"), "register pass did not run: {text:?}");
    }

    /// `prepared` must survive the round trip through Swift intact — a field lost in
    /// transit takes the gate's vocab with it and reads as a hallucination later.
    #[test]
    fn prepared_survives_the_round_trip() {
        let prepared = ok(serde_json::json!({
            "op": "prepare",
            "raw": "thanks monvi",
            "level": "light",
            "dictionary": dict(),
        }));
        assert!(
            prepared["ctx"]["vocab"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v["correct"] == "Manvi"),
            "vocab did not cross the bridge: {prepared}"
        );
    }

    /// A prefilter miss must say so, because the caller uses it to skip the second
    /// Whisper pass entirely — always prompting is the documented way to make
    /// Whisper emit words it never heard.
    #[test]
    fn asr_bias_prompt_is_null_when_nothing_matches() {
        let r = ok(serde_json::json!({
            "op": "asr_bias_prompt",
            "unprompted": "the weather is fine today",
            "dictionary": dict(),
        }));
        assert!(r["prompt"].is_null(), "primed a pass with no matching vocab: {r}");

        let r = ok(serde_json::json!({
            "op": "asr_bias_prompt",
            "unprompted": "call monvi back",
            "dictionary": dict(),
        }));
        assert!(r["prompt"].is_string(), "no prompt for a matching utterance: {r}");
    }

    #[test]
    fn asr_accept_prompted_rejects_a_rewrite() {
        let vocab = serde_json::json!([{"correct": "Manvi", "mishears": ["monvi"]}]);
        let accepted = ok(serde_json::json!({
            "op": "asr_accept_prompted",
            "unprompted": "call monvi",
            "prompted": "Call Manvi.",
            "vocab": vocab,
        }));
        assert_eq!(accepted, serde_json::Value::Bool(true));

        let rejected = ok(serde_json::json!({
            "op": "asr_accept_prompted",
            "unprompted": "call monvi",
            "prompted": "",
            "vocab": vocab,
        }));
        assert_eq!(rejected, serde_json::Value::Bool(false));
    }

    /// Swift must get a message, never a crash, for anything malformed.
    #[test]
    fn malformed_requests_are_errors_not_panics() {
        for bad in [
            "not json at all",
            r#"{"op": "no_such_op"}"#,
            r#"{"op": "prepare"}"#,
            r#"{"op": "finish", "prepared": {"model_input": "x"}}"#,
            "",
        ] {
            let v: serde_json::Value = serde_json::from_str(&handle(bad)).unwrap();
            assert_eq!(v["status"], "error", "{bad:?} should not have succeeded");
            assert!(v["message"].is_string());
        }
    }

    /// The pointer path, including the free. Under `cargo test` this runs the same
    /// allocator Xcode links, so a mismatched free would show up here too.
    #[test]
    fn the_c_entry_point_round_trips() {
        let request = CString::new(r#"{"op":"version"}"#).unwrap();
        unsafe {
            let out = whimpr_call(request.as_ptr());
            assert!(!out.is_null());
            let text = CStr::from_ptr(out).to_str().unwrap().to_string();
            whimpr_string_free(out);
            assert!(text.contains("\"status\":\"ok\""), "{text}");
        }
        // NULL in, error out — not a segfault.
        unsafe {
            let out = whimpr_call(std::ptr::null());
            let text = CStr::from_ptr(out).to_str().unwrap().to_string();
            whimpr_string_free(out);
            assert!(text.contains("NULL"), "{text}");
        }
        // Freeing NULL is a no-op.
        unsafe { whimpr_string_free(std::ptr::null_mut()) };
    }
}
