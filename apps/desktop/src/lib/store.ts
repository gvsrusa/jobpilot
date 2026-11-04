import { create } from "zustand";

export type View = "status" | "documents" | "data" | "profile" | "resumes" | "ai" | "applications" | "memory" | "agents" | "jobs";

type Ui = {
  view: View;
  setView(v: View): void;
  /** A job text the Resumes view opens on, set by Prepare on a job and read once. */
  resumeJob: string | null;
  openResumes(jobTextId: string): void;
  /** The apply run the run screen shows. Cleared when it is done. */
  applyRun: string | null;
  /** Whether the terminal panel hosts `applyRun`: only a run Start run made in this app
   * session. A run shown after a restart is view-only, never hosted and never cancelled. */
  applyHost: boolean;
  setApplyRun(id: string | null, host?: boolean): void;
};

export const useUiStore = create<Ui>((set) => ({
  view: "status",
  setView: (view) => set({ view }),
  resumeJob: null,
  openResumes: (jobTextId) => set({ view: "resumes", resumeJob: jobTextId }),
  applyRun: null,
  applyHost: false,
  setApplyRun: (applyRun, host = false) => set({ applyRun, applyHost: applyRun !== null && host }),
}));
