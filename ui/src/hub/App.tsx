import { useEffect, useState } from "react";
import { font } from "../tokens/values";
import { theme } from "./theme";
import { Onboarding } from "./Onboarding";
import { Sidebar, type Page } from "./Sidebar";
import { Home } from "./Home";
import { Insights } from "./Insights";
import { DictionaryPane } from "./DictionaryPane";
import { SettingsPane, modeLabel } from "./SettingsPane";
import { applyAppearance } from "./theme";
import { Help } from "./Help";
import {
  getSettings,
  setSettings,
  onSettingsChanged,
  getStatus,
  type Settings,
  type Status,
  DEFAULT_SETTINGS,
  EMPTY_STATUS,
} from "./api";

export function App() {
  const [page, setPage] = useState<Page>("home");
  const [settings, setLocalSettings] = useState<Settings>(DEFAULT_SETTINGS);
  const [entered, setEntered] = useState(false);
  const [status, setStatus] = useState<Status>(EMPTY_STATUS);

  const refresh = () => getStatus().then(setStatus);

  useEffect(() => {
    getSettings().then(setLocalSettings);
    refresh();
    // listen() resolves asynchronously, so a cleanup that runs before it settles
    // (StrictMode's double-mount) would otherwise leak a second listener.
    let cancelled = false;
    let unlisten: (() => void) | null = null;
    void onSettingsChanged(setLocalSettings).then((un) => {
      if (cancelled) un();
      else unlisten = un;
    });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  // Covers all three ways the value arrives: the initial load, a change made in
  // this window, and one made elsewhere that came in over the settings event.
  useEffect(() => {
    applyAppearance(settings.appearance);
  }, [settings.appearance]);

  const update = (s: Settings) => {
    setLocalSettings(s);
    void setSettings(s);
  };

  // Gate the app behind the setup wizard until the required permissions are granted.
  if (!(status.accessibility && status.microphone) && !entered) {
    return <Onboarding status={status} refresh={refresh} onEnter={() => setEntered(true)} />;
  }

  return (
    <div
      style={{
        display: "flex",
        height: "100vh",
        fontFamily: font.ui,
        color: theme.textBody,
        background: theme.pageBg,
      }}
    >
      <Sidebar page={page} setPage={setPage} engine={modeLabel(settings.cleanup_mode)} />
      <main style={{ flex: 1, minWidth: 0, overflowY: "auto" }}>
        <div style={{ padding: "36px 44px", margin: "0 auto", maxWidth: 1120 }}>
          {page === "home" && <Home triggerMode={settings.trigger_mode} />}
          {page === "insights" && <Insights />}
          {page === "dictionary" && <DictionaryPane />}
          {page === "settings" && (
            <SettingsPane settings={settings} onChange={update} status={status} refresh={refresh} />
          )}
          {page === "help" && <Help triggerMode={settings.trigger_mode} />}
        </div>
      </main>
    </div>
  );
}
