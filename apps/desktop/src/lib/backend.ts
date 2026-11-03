import type { AiStatus } from "@/lib/ipc";

/** A generation backend is one that can answer now: Ollama running, or an external provider that is on, available, consented, and able to carry a posting. */
export function backendEnabled(ai: AiStatus | undefined): boolean {
  if (!ai) return false;
  const ollama = ai.enabled.ollama && ai.ollama.state === "running";
  const claude = ai.enabled.claude_code && ai.claude_code.available && ai.claude_code.consent !== null;
  const external = ai.external.some(
    (p) => ai.enabled[p.name] && p.available && p.consent !== null && p.runs_job_text,
  );
  return ollama || claude || external;
}
