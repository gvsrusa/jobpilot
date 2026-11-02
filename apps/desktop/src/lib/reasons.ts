import { toAppError } from "./ipc";

/** Said wherever a suspended consent shows, Claude Code included. */
export const SUSPENDED =
  "Suspended: this consent was given before the terms on file now. Review it once to turn it back on.";

/** The endpoint's record is bound to its address as well as the terms. */
export const ENDPOINT_SUSPENDED =
  "Suspended: the endpoint address changed since this consent was given, or the terms on file did. Review it to turn it back on.";

// Task 20 reports the reason inside the message ("failed: not_logged_in"), so the message is
// searched for the stable word. ponytail: add a `reason` field to the error if a word ever
// has to be told from a provider's own text.
const REASONS: [word: string, text: string][] = [
  ["not_logged_in", "This CLI is not signed in. Run it once in your own terminal to sign in, then try again."],
  [
    "cli_account_refused",
    "Gemini CLI refused this account. Its plan may have moved to Antigravity CLI, which JobPilot does not run. A paid Gemini API key in the CLI may work, or use another provider.",
  ],
  ["cli_too_old", "The installed CLI is older than the version JobPilot checked. Update it, then try again."],
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
    "An AI provider is on, but your consent does not cover this task. On the AI screen, choose Review, then Allow what drafting needs.",
  ],
  [
    "consent_suspended",
    "Your consent for this provider was given before the terms on file now, so it is paused and nothing is sent. Review it once on the AI screen.",
  ],
  [
    "provider_unavailable",
    "An AI provider is on but cannot be reached. Check it on the AI screen, or write this one yourself.",
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
    "cli_error",
    "The AI program failed or answered in a way JobPilot could not use. Check it on the AI screen, or write this one yourself.",
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
    "no_key",
    "No key is stored for this provider. Add one on the AI screen, or write this one yourself.",
  ],
  [
    "no_api_model",
    "No model is chosen for this provider. Set one on the AI screen, or write this one yourself.",
  ],
  ["no_endpoint", "No endpoint address is set. Add one on the AI screen, or write this one yourself."],
  [
    "origin_refused",
    "That model is not eligible through OpenRouter. Use an openai/, anthropic/, or google/ model, not a free one, with no suffix but :nitro or :floor.",
  ],
  [
    "model_has_tools",
    "That model searches by itself or drives a computer, so JobPilot cannot keep it from tools while it reads a job posting. Choose one without search, research, or computer-use in its name, and without :online.",
  ],
  [
    "model_not_found",
    "The provider does not know that model id. Check the Model id on the AI screen, or the model chosen for Ollama.",
  ],
  ["model_refused", "The AI model declined this task, so nothing was generated. Try another model, or write this one yourself."],
  [
    "endpoint_changed",
    "The endpoint address changed since you gave consent, or the terms on file did, so nothing is sent. Review it on the AI screen.",
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
