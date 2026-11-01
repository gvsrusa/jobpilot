import type { KeyState } from "./ipc";

/** What `crypto::peek` says when no key was ever made: neither locked nor unlocked. */
export const NO_KEY_YET = "no_key_yet";

type Locked = Extract<KeyState, { state: "key_unavailable" | "key_missing" | "key_mismatch" }>;

/**
 * True only for a key problem the user can act on. `key_not_loaded` (not asked yet) and
 * no-key-yet say nothing about the key, so they are not locked.
 */
export function isLocked(key: KeyState | null | undefined): key is Locked {
  if (!key) return false;
  if (key.state === "key_unavailable") return key.reason !== NO_KEY_YET;
  return key.state === "key_missing" || key.state === "key_mismatch";
}

/** Import and discard are offered only when the key is gone or wrong, never when it is slow. */
export const offersImportAndDiscard = (key: KeyState | null | undefined) =>
  key?.state === "key_missing" || key?.state === "key_mismatch";

export function describeKey(key: KeyState | null | undefined): string {
  switch (key?.state) {
    case undefined:
      return "Not checked yet";
    case "key_not_loaded":
      return "Not checked yet";
    case "unlocked":
      return `Unlocked (key ${key.key_id})`;
    case "key_unavailable":
      return key.reason === NO_KEY_YET
        ? "No key yet. One is made when you first save a sensitive field."
        : "Locked: the keychain did not answer";
    case "key_missing":
      return `Locked: the key for this data (${key.key_id}) is not on this computer`;
    case "key_mismatch":
      return `Locked: the key found (${key.key_id}) does not match this data`;
  }
}
