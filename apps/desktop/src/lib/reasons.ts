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
    "Nothing to write from yet: approve some knowledge items first, in the Knowledge section of this screen",
  ],
  [
    "consent_needed",
    "Claude Code is on, but your consent does not cover this task. On the AI screen, choose Review consent, then Allow what drafting needs",
  ],
  [
    "provider_unavailable",
    "An AI provider is on but cannot be reached. Check the AI screen: start Ollama, or point JobPilot at claude",
  ],
  [
    "no_model_set",
    "No local model is chosen. Pick one on the AI screen, or write this one yourself",
  ],
  [
    "model_not_installed",
    "The chosen model is not installed in Ollama. Download it on the AI screen, or choose another",
  ],
  [
    "rate_limited",
    "The AI provider is busy and asked to wait. Try again shortly, or write this one yourself.",
  ],
  ["auth_failed", "The AI provider refused the key. Check it on the AI screen."],
  [
    "quota_exhausted",
    "The AI provider says your usage or credit has run out. Add credit with the vendor, then try again.",
  ],
  [
    "use_profile_by_country",
    "Work authorization is answered from your profile, for one country at a time. If this question asks something else, remove it and add it in other words",
  ],
  ["no_ai", "No AI provider could take this task, so nothing was generated"],
  ["no_embedding_model", "No embedding model is set on the AI screen"],
];

/** A plain-words hint for a failure reason, or null when the error holds none we know. */
export function explainReason(error: unknown): string | null {
  const { message } = toAppError(error);
  return REASONS.find(([word]) => message.includes(word))?.[1] ?? null;
}
