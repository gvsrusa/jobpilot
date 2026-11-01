import type { ReactNode } from "react";

/** Focusable so a view change can move focus here. */
export function PageHeading({ children }: { children: ReactNode }) {
  return (
    <h1 tabIndex={-1} className="text-xl font-semibold outline-none">
      {children}
    </h1>
  );
}
