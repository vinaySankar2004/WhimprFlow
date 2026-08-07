import { font } from "../tokens/values";
import { theme } from "./theme";
import { Card, PageTitle } from "./ui";
import type { TriggerMode } from "./api";

/**
 * The first two tips describe the dictation key, so they follow whichever
 * trigger mode is actually configured — Help that describes the other mode is
 * worse than no Help at all. Both tips still name the alternative and where to
 * switch it.
 */
function tips(mode: TriggerMode): { emoji: string; title: string; body: string }[] {
  const hold = mode === "hold";
  return [
    {
      emoji: "🎙️",
      title: hold ? "Hold to dictate" : "Press to start, press to stop",
      body: hold
        ? "Press and hold your dictation key (Fn by default), speak naturally, then release. Prefer not to hold a key down? Switch to press-to-start under Settings → Dictation Key. WhimprFlow transcribes on-device — nothing leaves your Mac unless you choose a cloud cleanup engine."
        : "Tap your dictation key (Fn by default) once to start listening, speak naturally, then tap it again to finish — there is nothing to hold down. Prefer holding the key instead? Switch back under Settings → Dictation Key. WhimprFlow transcribes on-device — nothing leaves your Mac unless you choose a cloud cleanup engine.",
    },
    {
      emoji: "✨",
      title: "Cleanup happens where your cursor is",
      body: `${hold ? "Release the key" : "Tap the key again"} and your cleaned-up text is typed straight into whatever app has focus — email, chat, notes, code. Choose how aggressive the cleanup is under Settings → Auto Cleanup.`,
    },
    {
      emoji: "⏹️",
      title: "Stop or throw one away",
      body: "While the pill is up, ■ stops the recording and pastes what you said so far, and ✕ — or the Esc key — discards the whole thing. Cancelling keeps working while it's still transcribing: nothing gets pasted and nothing is logged, right up until the text actually lands. Esc only means “cancel” while a dictation is live; the rest of the time WhimprFlow isn't watching your keyboard at all.",
    },
    {
      emoji: "🌐",
      title: "Fn opening the emoji picker?",
      body: "That's macOS, not WhimprFlow: the 🌐/Fn key has its own action, and our key listener never swallows it. Set System Settings → Keyboard → “Press 🌐 key to” → Do Nothing. The emoji picker is still on ⌃⌘Space, and Fn+F1–F12, Fn+arrows and Fn+Delete are unaffected.",
    },
    {
      emoji: "📖",
      title: "Teach it your vocabulary",
      body: 'Open Dictionary and add names, jargon, or acronyms it keeps mishearing. Add the correct spelling plus any "also heard as" variants and WhimprFlow will fix them automatically.',
    },
    {
      emoji: "🔑",
      title: "Pick a cleanup engine",
      body: "Under Settings → Cleanup Engine, run fully offline (Local), paste exactly what you said (Raw), or add an OpenAI / Anthropic key for cloud cleanup. Keys are stored in your macOS keychain.",
    },
  ];
}

export function Help({ triggerMode }: { triggerMode: TriggerMode }) {
  return (
    <div style={{ maxWidth: 720 }}>
      <PageTitle sub="A few tips to get the most out of WhimprFlow.">Help</PageTitle>
      <div style={{ display: "flex", flexDirection: "column", gap: 14 }}>
        {tips(triggerMode).map((t) => (
          <Card key={t.title}>
            <div style={{ display: "flex", gap: 14 }}>
              <div style={{ fontSize: 22, lineHeight: 1.2 }}>{t.emoji}</div>
              <div>
                <div
                  style={{
                    fontFamily: font.ui,
                    fontSize: 15,
                    fontWeight: 600,
                    color: theme.textStrong,
                    marginBottom: 4,
                  }}
                >
                  {t.title}
                </div>
                <div style={{ fontSize: 13.5, lineHeight: 1.55, color: theme.textMuted }}>{t.body}</div>
              </div>
            </div>
          </Card>
        ))}
      </div>
    </div>
  );
}
