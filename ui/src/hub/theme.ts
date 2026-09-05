// The Hub's theme, in both appearances.
//
// This governs only the desktop Hub window. The floating overlay pill stays on its
// own dark palette in every mode: it is drawn over whatever app is frontmost, so it
// is not part of a window whose background the user chose, and a light pill sitting
// on someone's dark editor reads as a rendering fault rather than a preference.
//
// # Why CSS variables and not a theme object per mode
//
// `theme.pageBg` is used in 184 places. Swapping a JS object per mode would mean
// threading a hook or context through every one of them, and every component that
// forgot would keep rendering the old palette — visible only in whichever mode
// nobody happened to be looking at.
//
// Instead each entry is a `var(--…)` reference, resolved by the browser. The call
// sites are unchanged, switching a mode is one attribute on <html>, and a component
// cannot opt out by accident. The one thing this rules out is reading a colour into
// JavaScript — `var()` is substituted at style-resolution time, so it is meaningless
// in a canvas context or an SVG *presentation attribute*. Set those through `style`
// instead, which is a CSS declaration and does substitute.

import { palette } from "../tokens/values";

export const theme = {
  // Surfaces
  pageBg: "var(--page-bg)",
  sidebarBg: "var(--sidebar-bg)",
  cardBg: "var(--card-bg)",
  cardBgSubtle: "var(--card-bg-subtle)",
  track: "var(--track)",
  hover: "var(--hover)",

  // Borders
  border: "var(--border)",
  borderStrong: "var(--border-strong)",

  // Text
  textStrong: "var(--text-strong)",
  textBody: "var(--text-body)",
  textMuted: "var(--text-muted)",
  textFaint: "var(--text-faint)",

  // Accent (teal/cyan — OUR trade dress)
  accent: "var(--accent)",
  accentDeep: "var(--accent-deep)",
  accentBright: "var(--accent-bright)",
  accentSoft: "var(--accent-soft)",
  accentSoftHover: "var(--accent-soft-hover)",
  accentSoftBorder: "var(--accent-soft-border)",

  // Elevation
  shadow: "var(--shadow)",
  shadowSoft: "var(--shadow-soft)",

  // Dark banner gradient
  bannerFrom: "var(--banner-from)",
  bannerVia: "var(--banner-via)",
  bannerTo: "var(--banner-to)",
} as const;

// The light values are the original Hub palette: warm light neutrals with our teal.
const light = {
  "--page-bg": "#F6F4EF",
  "--sidebar-bg": "#F1ECE3",
  "--card-bg": "#FFFFFF",
  "--card-bg-subtle": "#FBFAF7",
  "--track": "#ECE7DD",
  "--hover": "#F1EDE5",

  "--border": "#E7E1D6",
  "--border-strong": "#DAD3C6",

  "--text-strong": palette.slate900,
  "--text-body": palette.slate800,
  "--text-muted": palette.slate500,
  "--text-faint": palette.slate400,

  "--accent": palette.accent500,
  "--accent-deep": palette.accent600,
  "--accent-bright": palette.accent400,
  "--accent-soft": "rgba(34,195,182,0.12)",
  "--accent-soft-hover": "rgba(34,195,182,0.18)",
  "--accent-soft-border": "rgba(34,195,182,0.30)",

  "--shadow": "0 1px 2px rgba(17,20,25,0.04), 0 6px 20px rgba(17,20,25,0.05)",
  "--shadow-soft": "0 1px 2px rgba(17,20,25,0.05)",

  "--banner-from": palette.slate900,
  "--banner-via": palette.slate800,
  "--banner-to": palette.slate700,
};

// Dark is not the light palette inverted. Two things change on purpose:
//
// - The accent *brightens* (accent400 rather than accent500). The same teal that
//   carries on a warm off-white is muddy on near-black.
// - Shadows stop doing the work of separating surfaces, because a shadow is nearly
//   invisible against a dark ground. Cards separate by being *lighter* than the page
//   instead, which is why cardBg is above pageBg here and below it in light.
const dark = {
  "--page-bg": palette.slate950,
  "--sidebar-bg": palette.slate900,
  "--card-bg": palette.slate850,
  "--card-bg-subtle": palette.slate900,
  "--track": palette.slate700,
  "--hover": palette.slate800,

  "--border": "rgba(255,255,255,0.07)",
  "--border-strong": "rgba(255,255,255,0.14)",

  "--text-strong": palette.slate100,
  "--text-body": palette.slate200,
  "--text-muted": palette.slate400,
  "--text-faint": palette.slate500,

  "--accent": palette.accent400,
  "--accent-deep": palette.accent500,
  "--accent-bright": palette.accent400,
  "--accent-soft": "rgba(63,224,208,0.14)",
  "--accent-soft-hover": "rgba(63,224,208,0.22)",
  "--accent-soft-border": "rgba(63,224,208,0.34)",

  "--shadow": "0 1px 2px rgba(0,0,0,0.40), 0 6px 20px rgba(0,0,0,0.45)",
  "--shadow-soft": "0 1px 2px rgba(0,0,0,0.35)",

  // The banner already was a dark gradient; lift it off the page so it stays a
  // distinct band rather than merging into the background.
  "--banner-from": palette.slate800,
  "--banner-via": palette.slate700,
  "--banner-to": palette.slate600,
};

export type Appearance = "system" | "light" | "dark";

const block = (selector: string, vars: Record<string, string>) =>
  `${selector}{${Object.entries(vars)
    .map(([k, v]) => `${k}:${v}`)
    .join(";")}}`;

/// The stylesheet, written once at startup.
///
/// Light is the bare `:root` default so a document with no attribute set still
/// renders; the media query covers "system" without any JavaScript having to run,
/// which is what stops a flash of the wrong palette before React mounts.
export const themeCss = [
  block(":root", light),
  `@media (prefers-color-scheme: dark){${block(':root:not([data-appearance="light"])', dark)}}`,
  block(':root[data-appearance="dark"]', dark),
  block(':root[data-appearance="light"]', light),
  // `color-scheme` is what makes the scrollbars, form controls and the window's own
  // background follow along. Without it a dark Hub keeps light scrollbars.
  `:root{color-scheme:light dark}`,
  `:root[data-appearance="light"]{color-scheme:light}`,
  `:root[data-appearance="dark"]{color-scheme:dark}`,
].join("\n");

let injected = false;

/** Install the palette. Idempotent, so calling it from more than one entry point is safe. */
export function installTheme(): void {
  if (injected || typeof document === "undefined") return;
  const style = document.createElement("style");
  style.id = "whimpr-theme";
  style.textContent = themeCss;
  document.head.appendChild(style);
  injected = true;
}

/**
 * Apply an appearance.
 *
 * "system" removes the attribute rather than resolving the current preference, so the
 * media query stays in charge and the Hub follows the system *as it changes* instead
 * of freezing at whatever it was when this ran.
 */
export function applyAppearance(appearance: Appearance): void {
  if (typeof document === "undefined") return;
  const root = document.documentElement;
  if (appearance === "system") root.removeAttribute("data-appearance");
  else root.setAttribute("data-appearance", appearance);
}
