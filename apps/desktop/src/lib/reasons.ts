import { toAppError } from "./ipc";

// Task 20 reports the reason inside the message ("failed: not_logged_in"), so the message is
// searched for the stable word. ponytail: add a `reason` field to the error if a word ever
// has to be told from a provider's own text.
const REASONS: [word: string, text: string][] = [
  ["not_logged_in", "Sign in by running `claude` once in the terminal panel"],
  [
    "prompt_too_large",
    "The posting is too long for the model. Paste a shorter posting, such as just the duties and requirements",
  ],
  [
    "no_evidence",
    "Nothing to write from yet: approve some knowledge items first (in the terminal panel, `kb list` then `kb approve <id>`)",
  ],
  ["no_ai", "No AI provider is switched on, so nothing was generated"],
  ["no_embedding_model", "No embedding model is set on the AI screen"],
];

/** A plain-words hint for a failure reason, or null when the error holds none we know. */
export function explainReason(error: unknown): string | null {
  const { message } = toAppError(error);
  return REASONS.find(([word]) => message.includes(word))?.[1] ?? null;
}
