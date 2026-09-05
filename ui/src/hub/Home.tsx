import { useEffect, useMemo, useState } from "react";
import { font, palette } from "../tokens/values";
import { theme } from "./theme";
import { Card, useStats } from "./ui";
import { Icon } from "./icons";
import {
  EMPTY_HISTORY_PAGE,
  getHistory,
  type HistoryItem,
  type HistoryPage,
  type StatsSummary,
  type TriggerMode,
} from "./api";
import { dayKey, dayLabel, fmtCompact, fmtDuration, fmtNum, fmtTimeOfDay, wordsReference } from "./format";

const UNLOCK_WORDS = 500;

/** "Hold your key" is wrong advice in the other two modes, so the copy follows the
 *  setting — and double-tap mode is the one where a single tap does nothing at all. */
function startPhrase(mode: TriggerMode): string {
  if (mode === "hold") return "Hold your key";
  return mode === "double_tap" ? "Double-tap your key" : "Tap your key";
}

// ── Banner ───────────────────────────────────────────────────────────────────
function Banner({ triggerMode }: { triggerMode: TriggerMode }) {
  return (
    <div
      style={{
        position: "relative",
        overflow: "hidden",
        borderRadius: 16,
        padding: "26px 28px",
        background: `linear-gradient(135deg, ${theme.bannerFrom} 0%, ${theme.bannerVia} 52%, ${theme.bannerTo} 100%)`,
        boxShadow: theme.shadow,
      }}
    >
      {/* soft accent glow */}
      <div
        style={{
          position: "absolute",
          right: -60,
          top: -60,
          width: 220,
          height: 220,
          borderRadius: "50%",
          background: `radial-gradient(circle, ${palette.accentGlow} 0%, transparent 68%)`,
          opacity: 0.5,
          pointerEvents: "none",
        }}
      />
      <div style={{ position: "relative", maxWidth: 460 }}>
        <div
          style={{
            fontFamily: font.serif,
            fontSize: 23,
            fontWeight: 600,
            letterSpacing: -0.3,
            color: palette.slate050,
            lineHeight: 1.2,
          }}
        >
          Cleanup works anywhere you write.
        </div>
        <p style={{ color: palette.slate300, fontSize: 14, lineHeight: 1.55, margin: "10px 0 0" }}>
          {startPhrase(triggerMode)}, speak, and WhimprFlow types clean text wherever your cursor is.
        </p>
      </div>
    </div>
  );
}

// ── History ──────────────────────────────────────────────────────────────────
type Group = { key: string; label: string; items: HistoryItem[] };

function groupByDay(items: HistoryItem[]): Group[] {
  const now = new Date();
  const groups: Group[] = [];
  const index = new Map<string, Group>();
  for (const it of items) {
    const d = new Date(it.ts_unix * 1000);
    const k = dayKey(d);
    let g = index.get(k);
    if (!g) {
      g = { key: k, label: dayLabel(d, now), items: [] };
      index.set(k, g);
      groups.push(g);
    }
    g.items.push(it);
  }
  return groups;
}

function HistoryRow({ item }: { item: HistoryItem }) {
  const d = new Date(item.ts_unix * 1000);
  return (
    <div style={{ display: "flex", gap: 14, padding: "11px 4px", borderBottom: `1px solid ${theme.border}` }}>
      <div
        style={{
          flex: "0 0 74px",
          fontSize: 12,
          color: theme.textFaint,
          fontVariantNumeric: "tabular-nums",
          paddingTop: 1,
        }}
      >
        {fmtTimeOfDay(d)}
      </div>
      <div style={{ flex: 1, minWidth: 0 }}>
        <div style={{ fontSize: 13.5, lineHeight: 1.5, color: theme.textBody }}>{item.text}</div>
        {item.app && (
          <div style={{ fontSize: 11, color: theme.textFaint, marginTop: 3 }}>{item.app}</div>
        )}
      </div>
    </div>
  );
}

// ── Range filter ─────────────────────────────────────────────────────────────
// Ranges are resolved against the browser's local clock and sent to Rust as a
// plain Unix bound, so "today" means the user's midnight and no timezone logic is
// needed on the other side.
type RangeKey = "today" | "7d" | "30d" | "all";

const RANGES: { key: RangeKey; label: string }[] = [
  { key: "today", label: "Today" },
  { key: "7d", label: "7 days" },
  { key: "30d", label: "30 days" },
  { key: "all", label: "All" },
];

function sinceUnix(range: RangeKey): number {
  const midnight = new Date();
  midnight.setHours(0, 0, 0, 0);
  const day = 86_400;
  const startOfToday = Math.floor(midnight.getTime() / 1000);
  switch (range) {
    case "today":
      return startOfToday;
    case "7d":
      return startOfToday - 6 * day; // today plus the six days before it
    case "30d":
      return startOfToday - 29 * day;
    case "all":
      return 0;
  }
}

const PAGE_SIZE = 10;

function RangeTabs({ value, onChange }: { value: RangeKey; onChange: (r: RangeKey) => void }) {
  return (
    <div style={{ display: "flex", gap: 2, background: theme.cardBgSubtle, borderRadius: 9, padding: 2 }}>
      {RANGES.map((r) => {
        const active = r.key === value;
        return (
          <button
            key={r.key}
            onClick={() => onChange(r.key)}
            style={{
              border: "none",
              cursor: "pointer",
              fontFamily: font.ui,
              fontSize: 12,
              fontWeight: active ? 600 : 500,
              padding: "5px 10px",
              borderRadius: 7,
              background: active ? theme.cardBg : "transparent",
              color: active ? theme.textStrong : theme.textMuted,
              boxShadow: active ? theme.shadowSoft : "none",
            }}
          >
            {r.label}
          </button>
        );
      })}
    </div>
  );
}

function Pager({
  offset,
  total,
  onOffset,
}: {
  offset: number;
  total: number;
  onOffset: (n: number) => void;
}) {
  const first = offset + 1;
  const last = Math.min(offset + PAGE_SIZE, total);
  const btn = (enabled: boolean) =>
    ({
      display: "flex",
      alignItems: "center",
      gap: 4,
      border: `1px solid ${theme.border}`,
      borderRadius: 8,
      background: theme.cardBg,
      color: enabled ? theme.textBody : theme.textFaint,
      cursor: enabled ? "pointer" : "default",
      opacity: enabled ? 1 : 0.45,
      fontFamily: font.ui,
      fontSize: 12.5,
      padding: "5px 10px",
    }) as const;

  const hasPrev = offset > 0;
  const hasNext = last < total;

  return (
    <div
      style={{
        display: "flex",
        alignItems: "center",
        justifyContent: "space-between",
        padding: "12px 4px 2px",
        borderTop: `1px solid ${theme.border}`,
        marginTop: 8,
      }}
    >
      <div style={{ fontSize: 12, color: theme.textFaint, fontVariantNumeric: "tabular-nums" }}>
        {first}–{last} of {fmtNum(total)}
      </div>
      <div style={{ display: "flex", gap: 8 }}>
        <button
          style={btn(hasPrev)}
          disabled={!hasPrev}
          onClick={() => onOffset(Math.max(0, offset - PAGE_SIZE))}
        >
          Previous
        </button>
        <button style={btn(hasNext)} disabled={!hasNext} onClick={() => onOffset(offset + PAGE_SIZE)}>
          Next
        </button>
      </div>
    </div>
  );
}

function HistorySection({ triggerMode }: { triggerMode: TriggerMode }) {
  const [query, setQuery] = useState("");
  const [range, setRange] = useState<RangeKey>("7d");
  const [offset, setOffset] = useState(0);
  const [page, setPage] = useState<HistoryPage>(EMPTY_HISTORY_PAGE);

  // Debounced so a fast typist doesn't fire a query per keystroke.
  const [debounced, setDebounced] = useState("");
  useEffect(() => {
    const id = setTimeout(() => setDebounced(query.trim()), 200);
    return () => clearTimeout(id);
  }, [query]);

  // Any change to what is being *asked for* returns to the first page; staying on
  // page 4 of a new, shorter result set shows a confusing empty table.
  useEffect(() => setOffset(0), [debounced, range]);

  const since = useMemo(() => sinceUnix(range), [range]);

  useEffect(() => {
    let alive = true;
    const load = () =>
      getHistory({ search: debounced, since_unix: since, offset, limit: PAGE_SIZE }).then(
        (p) => alive && setPage(p),
      );
    void load();
    // Poll so a dictation made while the Hub is open appears without a refresh.
    const id = setInterval(load, 8000);
    return () => {
      alive = false;
      clearInterval(id);
    };
  }, [debounced, since, offset]);

  const groups = groupByDay(page.items);

  return (
    <Card pad={0}>
      <div
        style={{
          display: "flex",
          alignItems: "center",
          justifyContent: "space-between",
          gap: 12,
          flexWrap: "wrap",
          padding: "16px 18px",
          borderBottom: `1px solid ${theme.border}`,
        }}
      >
        <div
          style={{
            fontSize: 11.5,
            fontWeight: 700,
            letterSpacing: 0.7,
            textTransform: "uppercase",
            color: theme.textFaint,
          }}
        >
          Recent dictations
        </div>
        <div style={{ display: "flex", alignItems: "center", gap: 10 }}>
          <RangeTabs value={range} onChange={setRange} />
          <div
            style={{
              display: "flex",
              alignItems: "center",
              gap: 7,
              background: theme.cardBgSubtle,
              border: `1px solid ${theme.border}`,
              borderRadius: 9,
              padding: "6px 10px",
              minWidth: 180,
            }}
          >
            <Icon name="search" size={15} style={{ color: theme.textFaint }} />
            <input
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              placeholder="Search history"
              style={{
                border: "none",
                outline: "none",
                background: "transparent",
                fontFamily: font.ui,
                fontSize: 13,
                color: theme.textBody,
                width: "100%",
              }}
            />
          </div>
        </div>
      </div>

      <div style={{ padding: "6px 18px 14px" }}>
        {page.items.length === 0 ? (
          <div style={{ padding: "36px 8px", textAlign: "center", color: theme.textFaint, fontSize: 13.5 }}>
            {/* Each branch says only what this query actually establishes. Only an
                unfiltered, all-time empty result proves there is nothing yet. */}
            {debounced
              ? `No dictations match “${debounced}”${range === "all" ? "" : " in this range"}.`
              : range !== "all"
                ? "Nothing dictated in this range."
                : `Your dictations will show up here. ${startPhrase(triggerMode)} and start speaking.`}
          </div>
        ) : (
          <>
            {groups.map((g) => (
              <div key={g.key} style={{ marginTop: 14 }}>
                <div
                  style={{
                    fontSize: 11,
                    fontWeight: 700,
                    letterSpacing: 0.6,
                    textTransform: "uppercase",
                    color: theme.accentDeep,
                    marginBottom: 2,
                  }}
                >
                  {g.label}
                </div>
                {g.items.map((it, i) => (
                  <HistoryRow key={`${it.ts_unix}-${i}`} item={it} />
                ))}
              </div>
            ))}
            <Pager offset={offset} total={page.total} onOffset={setOffset} />
          </>
        )}
      </div>
    </Card>
  );
}

// ── Stats card (right column) ────────────────────────────────────────────────
function BigStat({ value, label, accent }: { value: string; label: string; accent?: boolean }) {
  return (
    <div style={{ flex: 1, textAlign: "center" }}>
      <div
        style={{
          fontFamily: font.serif,
          fontSize: 30,
          fontWeight: 600,
          lineHeight: 1.05,
          color: accent ? theme.accentDeep : theme.textStrong,
        }}
      >
        {value}
      </div>
      <div
        style={{
          fontSize: 10.5,
          color: theme.textFaint,
          marginTop: 6,
          textTransform: "uppercase",
          letterSpacing: 0.6,
        }}
      >
        {label}
      </div>
    </div>
  );
}

function StatsCard({ stats }: { stats: StatsSummary }) {
  const unlocked = stats.total_words >= UNLOCK_WORDS;
  return (
    <Card>
      <div style={{ display: "flex", justifyContent: "space-between", alignItems: "baseline", marginBottom: 4 }}>
        <div style={{ fontSize: 14, fontWeight: 600, color: theme.textStrong }}>Your stats</div>
        <div style={{ fontSize: 12, color: theme.accentDeep, fontWeight: 600 }}>🔥 keep it up</div>
      </div>

      <div style={{ textAlign: "center", margin: "16px 0 6px" }}>
        <div style={{ fontFamily: font.serif, fontSize: 42, fontWeight: 600, color: theme.textStrong, lineHeight: 1 }}>
          {fmtCompact(stats.total_words)}
        </div>
        <div style={{ fontSize: 11.5, color: theme.textFaint, marginTop: 6, textTransform: "uppercase", letterSpacing: 0.6 }}>
          total words
        </div>
      </div>

      <div style={{ fontSize: 12, color: theme.textMuted, textAlign: "center", marginBottom: 16 }}>
        {wordsReference(stats.total_words)}
      </div>

      <div
        style={{
          display: "flex",
          gap: 8,
          padding: "16px 0 0",
          borderTop: `1px solid ${theme.border}`,
        }}
      >
        <BigStat value={fmtNum(stats.avg_wpm)} label="avg WPM" accent />
        <BigStat value={`${stats.day_streak}`} label="day streak" />
      </div>

      {unlocked ? (
        <div style={{ fontSize: 12, color: theme.textFaint, textAlign: "center", marginTop: 14 }}>
          {fmtNum(stats.best_wpm)} WPM best · saved you {fmtDuration(stats.time_saved_secs)} vs typing
        </div>
      ) : (
        <div style={{ fontSize: 12, color: theme.textFaint, textAlign: "center", marginTop: 14, lineHeight: 1.5 }}>
          Keep dictating to unlock richer stats — {fmtNum(Math.max(0, UNLOCK_WORDS - stats.total_words))} words to go.
        </div>
      )}
    </Card>
  );
}

// ── Page ─────────────────────────────────────────────────────────────────────
export function Home({ triggerMode }: { triggerMode: TriggerMode }) {
  const stats = useStats();
  const today = stats.words_today;

  return (
    <div style={{ maxWidth: 1000 }}>
      <div style={{ marginBottom: 22 }}>
        <h1
          style={{
            fontFamily: font.serif,
            fontSize: 32,
            fontWeight: 600,
            letterSpacing: -0.5,
            margin: 0,
            color: theme.textStrong,
          }}
        >
          Welcome back
        </h1>
        <p style={{ color: theme.textMuted, fontSize: 14, margin: "8px 0 0" }}>
          {today > 0
            ? `${fmtNum(today)} words dictated today.`
            : `Ready when you are — ${startPhrase(triggerMode).toLowerCase()} and speak.`}
        </p>
      </div>

      <div style={{ display: "flex", flexWrap: "wrap", gap: 22, alignItems: "flex-start" }}>
        <div style={{ flex: "1 1 440px", minWidth: 0, display: "flex", flexDirection: "column", gap: 22 }}>
          <Banner triggerMode={triggerMode} />
          <HistorySection triggerMode={triggerMode} />
        </div>
        <div style={{ flex: "0 0 300px", width: 300, maxWidth: "100%" }}>
          <StatsCard stats={stats} />
        </div>
      </div>
    </div>
  );
}
