import { create } from "zustand";

export type View = "status" | "documents" | "data" | "profile" | "resumes" | "ai" | "applications" | "memory" | "agents" | "jobs";

type Ui = {
  view: View;
  setView(v: View): void;
  /** A job text the Resumes view opens on, set by Prepare on a job and read once. */
  resumeJob: string | null;
  openResumes(jobTextId: string): void;
  /** The apply run the terminal panel hosts. Set by Start run, cleared when it is done. */
  applyRun: string | null;
  setApplyRun(id: string | null): void;
};

export const useUiStore = create<Ui>((set) => ({
  view: "status",
  setView: (view) => set({ view }),
  resumeJob: null,
  openResumes: (jobTextId) => set({ view: "resumes", resumeJob: jobTextId }),
  applyRun: null,
  setApplyRun: (applyRun) => set({ applyRun }),
}));
