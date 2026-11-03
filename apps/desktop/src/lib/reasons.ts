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
    "unusable_reply",
    "The AI model answered, but not in a form JobPilot could use, so nothing was saved. Try again, choose a larger model, or write this one yourself.",
  ],
  [
    "output_truncated",
    "The AI model ran out of room before it finished, so nothing was saved. Remove some knowledge items, shorten the posting, or write this one yourself.",
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
  ["policy_unknown", "The policy register has no clear answer for this source, so nothing was requested."],
  ["policy_blocked", "The policy register blocks reading this source, so nothing was requested."],
  ["policy_user_assisted", "This source is user-assisted only. Import a posting by hand instead."],
  ["operation_not_declared", "This source does not declare that operation, so nothing was requested."],
  [
    "source_blocked",
    "The source refused or showed a challenge page. JobPilot stops there, tries nothing else, and leaves it alone for a day.",
  ],
  ["source_not_found", "The source says that board or feed does not exist. Check the name."],
  ["bad_feed", "The source answered in a shape JobPilot does not read, so nothing was stored."],
  ["needs_text", "That posting has no text to read. Import it with its text instead."],
  ["network", "The source could not be reached. Check the connection and try again."],
  ["too_large", "The answer was over the size limit, so it was not stored."],
  ["timeout", "It took too long to answer. Try again later."],
  ["no_sources", "This company has no stored text to read yet. Import a posting or paste a note first."],
  ["company_gone", "This company no longer exists, so nothing was saved."],
  ["no_ai", "No AI provider could take this task, so nothing was generated"],
  ["no_embedding_model", "No embedding model is set on the AI screen"],
  ["no_browser_runtime", "The browser runtime is not set up. Press Set up browser, then start the run again."],
  ["claude_not_found", "Claude Code was not found. Install it, or choose its program on the AI screen."],
  ["apply_run_active", "Another apply run is still open. Let it end, or cancel it, then start this one."],
  ["apply_run_not_ready", "This run has already started or ended. Start a new run with what is left."],
  ["consent_required", "Your Claude Code consent does not cover what this run sends. Review it on the AI screen."],
  [
    "release_changed",
    "Your sealed answers changed while the release was being asked, so nothing started. Check the preflight, then start again.",
  ],
  [
    "claude_exited",
    "Claude Code stopped with an error, so the run ended. It may be signed out: run it once in your own terminal, then start a new run.",
  ],
];

/** Why an application cannot go in a run, from the preflight's block codes. */
const BLOCKS: Record<string, string> = {
  not_saved: "Only an application with status Saved can go in a run.",
  link_not_https: "The posting link is not https.",
  already_in_a_run: "It is already in a run that has not ended.",
  resume_blocked: "The resume has a claim to fix before it can be exported.",
  letter_blocked: "The cover letter has a claim to fix before it can be exported.",
  already_submitted: "This application was already submitted, or may have been. Record its outcome first.",
};

export function blockText(code: string): string {
  return BLOCKS[code] ?? `Refused: ${code}`;
}

/** Why an item ended where it did, from its reason code. */
const ITEM_REASONS: Record<string, string> = {
  session_ended: "The session ended while this application was open.",
  run_ended: "The run ended before this application was reached.",
  claude_exited: "Claude Code stopped with an error while this application was open.",
};

export function itemReason(code: string): string {
  return ITEM_REASONS[code] ?? code;
}

/** A plain-words hint for a failure reason, or null when the error holds none we know. */
export function explainReason(error: unknown): string | null {
  const { message } = toAppError(error);
  return REASONS.find(([word]) => message.includes(word))?.[1] ?? null;
}
