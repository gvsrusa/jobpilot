import "@testing-library/jest-dom/vitest";
import { cleanup } from "@testing-library/react";
import { clearMocks } from "@tauri-apps/api/mocks";
import { afterEach } from "vitest";
import { useUiStore } from "./lib/store";

// jsdom has no matchMedia, and xterm.js watches the device pixel ratio with it.
window.matchMedia ??= (query: string) =>
  ({
    matches: false,
    media: query,
    addListener: () => undefined,
    removeListener: () => undefined,
    addEventListener: () => undefined,
    removeEventListener: () => undefined,
  }) as unknown as MediaQueryList;

afterEach(async () => {
  cleanup();
  // The terminal panel closes its session after unmount; let that finish against the mocks.
  await new Promise((resolve) => setTimeout(resolve));
  clearMocks();
  // A run one test started must not open the next test's terminal.
  useUiStore.setState({ applyRun: null });
});
