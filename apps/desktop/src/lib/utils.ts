export { cn } from "cn"

export function formatDate(iso: string): string {
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return iso;
  return date.toLocaleDateString(undefined, { month: "short", day: "numeric", year: "numeric" });
}

/** Last path segment, for backup names. Handles both separators. */
export function baseName(path: string): string {
  return path.split(/[\\/]/).filter(Boolean).pop() ?? path;
}

/** A date-only value (YYYY-MM-DD) as written, in the user's locale. `new Date("2026-10-02")`
 * is UTC midnight, which reads as the day before west of Greenwich. */
export function formatDay(day: string): string {
  const match = /^(\d{4})-(\d{2})-(\d{2})$/.exec(day);
  if (!match) return day;
  const date = new Date(Number(match[1]), Number(match[2]) - 1, Number(match[3]));
  return date.toLocaleDateString(undefined, { month: "short", day: "numeric", year: "numeric" });
}
