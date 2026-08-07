//! What macOS itself does when the 🌐/Fn key is pressed and released on its own.
//!
//! Nothing to do with dictation, and everything to do with why a new user's first
//! Fn press pops up the emoji picker. Our event tap is listen-only by design: a
//! consuming tap could swallow the Fn flag change, but it would take Fn+F1–F12,
//! Fn+arrows and Fn+Delete with it. So macOS's own action always fires alongside
//! ours, and the only real fix is the system setting — which means the app has to
//! be able to read it in order to point the user at it.
//!
//! The setting lives in the **`com.apple.HIToolbox`** domain under
//! `AppleFnUsageType`, not in NSGlobalDomain where the name suggests. Verified by
//! reading both on a live machine; do not "fix" this to `-g` without re-checking.
//! It is also frequently *absent*, which is not the same as "Do Nothing" — an
//! absent key means the macOS default, which on Apple keyboards is the emoji
//! picker. Reading it with an integer default of 0 would report the exact opposite
//! of the truth, so the absent case is distinguished explicitly.

/// The action macOS performs on a lone Fn press, as a stable string for the UI:
/// `"do_nothing"` | `"input_source"` | `"emoji"` | `"dictation"` | `"unknown"`.
/// Anything other than `"do_nothing"` fires on top of dictation.
pub fn fn_key_action() -> &'static str {
    match imp::apple_fn_usage_type() {
        Some(0) => "do_nothing",
        Some(1) => "input_source",
        Some(2) => "emoji",
        Some(3) => "dictation",
        // Absent (macOS default — the emoji picker on Apple keyboards) or a value
        // this build doesn't know. Either way it is not "off", so say so honestly
        // rather than guessing at a label.
        _ => "unknown",
    }
}

mod imp {
    use std::os::raw::c_void;

    type CFStringRef = *const c_void;
    type CFTypeRef = *const c_void;
    type CFTypeID = usize;

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFPreferencesCopyAppValue(key: CFStringRef, application_id: CFStringRef) -> CFTypeRef;
        /// Drops cfprefsd's cached copy for this domain, so a change the user just
        /// made in System Settings is visible to the next read rather than minutes
        /// later. The Hub polls this while the setup wizard is open.
        fn CFPreferencesAppSynchronize(application_id: CFStringRef);
        fn CFGetTypeID(cf: CFTypeRef) -> CFTypeID;
        fn CFNumberGetTypeID() -> CFTypeID;
        fn CFNumberGetValue(number: CFTypeRef, the_type: i32, value: *mut c_void) -> bool;
        fn CFRelease(cf: CFTypeRef);
    }

    /// `kCFNumberSInt64Type`.
    const K_CF_NUMBER_SINT64: i32 = 4;

    const DOMAIN: &str = "com.apple.HIToolbox";
    const KEY: &str = "AppleFnUsageType";

    /// The raw preference value, or `None` when the key is absent or not a number.
    pub fn apple_fn_usage_type() -> Option<i64> {
        use objc2_foundation::NSString;

        let domain = NSString::from_str(DOMAIN);
        let key = NSString::from_str(KEY);
        // NSString is toll-free bridged to CFString, so the pointers are usable as-is.
        let domain_ref = &*domain as *const NSString as CFStringRef;
        let key_ref = &*key as *const NSString as CFStringRef;

        // SAFETY: both pointers are live for the call, and the returned value is
        // owned by us (Copy rule) — released before returning either way.
        unsafe {
            CFPreferencesAppSynchronize(domain_ref);
            let value = CFPreferencesCopyAppValue(key_ref, domain_ref);
            if value.is_null() {
                return None;
            }
            let out = if CFGetTypeID(value) == CFNumberGetTypeID() {
                let mut n: i64 = 0;
                CFNumberGetValue(value, K_CF_NUMBER_SINT64, &mut n as *mut i64 as *mut c_void)
                    .then_some(n)
            } else {
                None
            };
            CFRelease(value);
            out
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Whatever this machine is set to, the answer is one of the known strings —
    /// the UI switches on it, and an unexpected value would silently show nothing.
    #[test]
    fn reports_a_known_action() {
        assert!(matches!(
            fn_key_action(),
            "do_nothing" | "input_source" | "emoji" | "dictation" | "unknown"
        ));
    }
}
