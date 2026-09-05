# Getting WhimprFlow onto a phone

Two audiences, two procedures:

- **[Part 1](#part-1--onto-your-own-phone)** — you, via Xcode and a cable. ~20 minutes,
  and the only way to test the keyboard at all.
- **[Part 2](#part-2--onto-someone-elses-phone)** — mom, via TestFlight. Do Part 1
  first; TestFlight cannot tell you *why* something is broken.

Everything below uses team **VSTTF2AM22** (Apple ID `vinay.sankara@gmail.com`) — the
paid team that holds the Developer ID. **Not** 3V3J78V32Q, which owns the older Apple
Development certs on this machine; mixing them fails with a 403 about "a required
agreement is missing or has expired", which is not what it sounds like.

> Apple moves things around in the portal and in App Store Connect. Where a label here
> does not match what you see, the *concept* is still right — find the equivalent
> rather than assuming the step is obsolete.

---

## Part 1 — onto your own phone

### 1. Check whether the App IDs already exist

**Look before you create.** Building the project once with automatic signing makes
Xcode register both App IDs for you, named with an `XC ` prefix (`XC com whimpr
whimprflow`). If you then try to register them by hand, Apple answers *"An App ID with
Identifier 'com.whimpr.whimprflow' is not available. Please enter a different
string."* — which sounds like somebody else owns it, and means you already do.

1. Go to <https://developer.apple.com/account/resources/identifiers/list>.
2. Check the team selector (top right) says **VSTTF2AM22**.
3. Look for `com.whimpr.whimprflow` and `com.whimpr.whimprflow.keyboard`.

**If both are listed, skip to step 3** — Xcode registering an App ID does *not*
configure App Groups on it, so there is still work to do.

Only if one is missing, create it: **+** → **App IDs** → **App** → Continue.

- Description `WhimprFlow`, Bundle ID **Explicit** `com.whimpr.whimprflow`
- Under Capabilities tick **App Groups** → Continue → Register.
- Repeat with `WhimprFlow Keyboard` / `com.whimpr.whimprflow.keyboard`.

> Still told "not available" while it appears on neither list? Then it is registered
> to another team — check your other team (3V3J78V32Q) with the team selector before
> concluding it belongs to a stranger. Only a genuine outside collision needs a new
> bundle id, and that means changing it in `ios/project.yml` (both targets), both
> `.entitlements` files, `Info.plist`, `Shared/Handoff.swift` and
> `Shared/Settings.swift` together.

### 2. Create the App Group

This is the only channel between the keyboard and the app. Get it wrong and the
keyboard reads an empty container — silently, with no error.

1. Identifiers → the dropdown at top right that says **App IDs** → change it to **App
   Groups**.
2. If `group.com.whimpr.whimprflow` is already there, skip to step 3. (Xcode creates
   it in some situations and not others; either is fine.)
3. **+** → Description `WhimprFlow Shared` → Identifier **`group.com.whimpr.whimprflow`**
   — exactly this string, matching both `.entitlements` files character for character
   → Continue → Register.

### 3. Assign the group to both App IDs

**Do not skip this because the App IDs already existed.** Registering an App ID —
whether you did it or Xcode did — attaches no groups, and neither does registering the
group. This step is what connects them, and omitting it on *either* App ID leaves the
keyboard reading an empty container with nothing logged anywhere.

1. Identifiers → back to **App IDs** → click `com.whimpr.whimprflow`.
2. Find **App Groups** in the capability list. Tick **Enable** if it is not already,
   then click **Configure** (or **Edit**).
3. Tick `group.com.whimpr.whimprflow` → Continue → Save.
4. **Repeat for `com.whimpr.whimprflow.keyboard`.** Both, or the handoff is one-way.

### 4. Open the project

```bash
cd ios && xcodegen generate && open WhimprFlow.xcodeproj
```

The Rust core builds automatically as a pre-build phase — nothing extra to run.

### 5. Check signing

For each of the two targets (**WhimprFlow** and **WhimprKeyboard**), select it in the
sidebar → **Signing & Capabilities**:

- **Automatically manage signing**: ticked
- **Team**: the VSTTF2AM22 team
- No red errors. Xcode fetches the profiles it needs from what you registered above.

If it complains the App Group is not in the profile, you missed step 3 for that
target. Fix it in the portal, then Xcode → **Product ▸ Clean Build Folder**.

### 6. Run it

1. Plug the iPhone in. Trust the Mac if asked.
2. Pick your phone in the destination selector at the top of the Xcode window.
3. **⌘R**.

First launch on a device you have not used for development: iOS refuses to run it
until you approve the certificate. On the phone, **Settings ▸ General ▸ VPN & Device
Management ▸ Developer App**, tap your Apple ID, **Trust**. Then run again.

### 7. Set it up on the phone

1. Open WhimprFlow. Allow the microphone.
2. **⚙︎ ▸ Groq API key** → paste a key from <https://console.groq.com/keys> → **Save
   key** → **Check connection**. It should say *reachable*. If it says *key rejected*,
   the key is wrong; anything else and the network is.
3. Add the keyboard: **Settings ▸ General ▸ Keyboard ▸ Keyboards ▸ Add New
   Keyboard…** → under THIRD-PARTY KEYBOARDS, **WhimprFlow**.
4. **Turn on Allow Full Access** — tap WhimprFlow in that same keyboards list, toggle
   it, accept the warning. **Without this nothing works**: the keyboard gets no
   network and cannot see the app's results.

### 8. Try it

Open Messages, tap a text field, hold **🌐** and pick WhimprFlow. Tap the mic key,
speak, tap it again. The text should land at the cursor.

If the mic key opens the WhimprFlow app instead of recording in place, that is the
designed fallback: iOS had killed the backgrounded app. Tap the back arrow when it is
done. Opening WhimprFlow once and returning primes it.

---

## Part 2 — onto someone else's phone

TestFlight. No App Store review, no public listing, and installing is one tap for them.

### 1. Create the app record

1. <https://appstoreconnect.apple.com> → **Apps** → **+** → **New App**.
2. Platform **iOS**; Bundle ID `com.whimpr.whimprflow`; SKU anything (`whimprflow`);
   Access **Full**.
3. **Name** must be unique across the entire App Store, even for a TestFlight-only
   app. If `WhimprFlow` is taken, use something like `WhimprFlow Dictation` — this is
   only the App Store Connect listing name and does not change what shows under the
   icon on the phone, which comes from `CFBundleDisplayName`.

### 2. Add the testers as users

Internal testers must be users on your App Store Connect team. This is what avoids
Beta App Review entirely.

1. **Users and Access** → **+**.
2. Their name and **the Apple ID email they actually use on their iPhone**. Getting
   this wrong is the most common failure — the invite lands somewhere they never see.
3. Role: **Customer Support** is enough, and is the least access that works. Do not
   grant Admin.
4. They get an email and must accept before you can add them to a tester group.

### 3. Upload a build

1. In Xcode, set the destination to **Any iOS Device (arm64)** — you cannot archive
   with a simulator selected.
2. **Product ▸ Archive**.
3. When the Organizer opens: **Distribute App** → **TestFlight & App Store** →
   **Upload** → accept the defaults → Upload.
4. Wait. The build shows as "Processing" in App Store Connect for anywhere from five
   minutes to about half an hour.

**Bump the build number before every upload.** In `ios/project.yml`, increment
`CURRENT_PROJECT_VERSION`, then `xcodegen generate`. App Store Connect rejects a build
whose version/build pair it has seen — and it tells you *after* the upload finishes.

### 4. Give the build to the testers

1. App Store Connect → your app → **TestFlight**.
2. **Internal Testing** → **+** next to Groups → name it `Family` → Create.
3. Add the people you invited in step 2.
4. Under **Builds**, add the processed build to that group.

They get an email immediately. No review, no waiting.

### 5. What they do

Send them this:

> 1. Install **TestFlight** from the App Store (it is Apple's own app, free).
> 2. Open the invite email on your iPhone and tap **View in TestFlight**, or open
>    TestFlight and use the redeem code from the email.
> 3. Tap **Install**. WhimprFlow appears on your home screen like a normal app.
> 4. Open it and allow the microphone.
> 5. Get a free API key at <https://console.groq.com/keys> — sign in, **Create API
>    Key**, copy it. Paste it into WhimprFlow under ⚙︎, tap **Save key**, then **Check
>    connection**.
> 6. To dictate inside other apps: **Settings ▸ General ▸ Keyboard ▸ Keyboards ▸ Add
>    New Keyboard…** and choose **WhimprFlow**. Then tap WhimprFlow in that list and
>    turn on **Allow Full Access** — it will not work without this.
> 7. In any app, hold the 🌐 key on the keyboard and pick WhimprFlow. Tap the mic,
>    speak, tap it again.

Each person needs their own Groq key. They are free, and the key is stored in their
phone's Keychain — it never reaches you.

### 6. Every 90 days

TestFlight builds expire. When one does, testers see "This build is no longer
available" and dictation stops.

Bump `CURRENT_PROJECT_VERSION`, archive, upload, add it to the group. Four times a
year. Setting a calendar reminder for day 80 is cheaper than being told it broke.

---

## When something does not work

| Symptom | Cause |
|---|---|
| Mic key does nothing, no app switch | Allow Full Access is off. It is a *separate* toggle from adding the keyboard, and adding the keyboard does not imply it. |
| Mic key always opens the app | Expected when iOS has killed the backgrounded app; it is the fallback. If it is constant, check ⚙︎ ▸ "Keep the mic ready in the background" is on. |
| Text never appears after dictating | The App Group is not attached to *both* App IDs (Part 1, step 3). The keyboard reads an empty container and nothing errors. |
| "Groq rejected the API key" | Wrong or revoked key. Re-copy from the Groq console; keys are shown once. |
| Rate limited | The free tier's daily cap. It resets. |
| Key will not save | Building unsigned. Never pass `CODE_SIGNING_ALLOWED=NO` — the app then has no `application-identifier` and every Keychain write fails. |
| Upload rejected, "redundant version" | You did not bump `CURRENT_PROJECT_VERSION`. |
| Build stuck in "Missing Compliance" | `ITSAppUsesNonExemptEncryption` did not make it into the build. It is in `Info.plist`; check the archive really contains it. |

Dictation never hard-fails on the network: a cloud error, a truncated reply or a gate
rejection all fall back to the raw transcript, with the reason shown under the result.
Silence with no message means recording, not cleanup.
