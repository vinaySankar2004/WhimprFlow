// The C ABI over whimpr-core. See crates/whimpr-ffi/src/lib.rs for the contract and
// the request/response shapes; this header is only the two symbols Xcode links.
//
// Hand-written rather than generated: it is two functions, and a cbindgen step in the
// build would be one more thing to keep installed for no benefit at this size.

#ifndef WHIMPR_H
#define WHIMPR_H

#ifdef __cplusplus
extern "C" {
#endif

/// Call the core with a JSON request; returns a JSON response.
///
/// `request` may be NULL (answered with an error response, not a crash) and is not
/// retained past the call. The result is never NULL, is always NUL-terminated UTF-8,
/// and is the caller's to release with `whimpr_string_free` — and with nothing else,
/// since it was allocated by Rust's allocator.
///
/// Responses are `{"status":"ok","result":...}` or `{"status":"error","message":...}`.
/// A panic inside the core is caught and returned as the latter.
char *whimpr_call(const char *request);

/// Release a string returned by `whimpr_call`. NULL is a no-op. Passing a pointer
/// from anywhere else, or the same pointer twice, is undefined.
void whimpr_string_free(char *ptr);

#ifdef __cplusplus
}
#endif

#endif /* WHIMPR_H */
