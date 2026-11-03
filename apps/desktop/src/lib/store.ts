import { create } from "zustand";

export type View = "status" | "documents" | "data" | "profile" | "resumes" | "ai" | "applications" | "memory" | "agents" | "jobs";

type Ui = {
  view: View;
  setView(v: View): void;
  /** A job text the Resumes view opens on, set by Prepare on a job and read once. */
  resumeJob: string | null;
  openResumes(jobTextId: string): void;
};

export const useUiStore = create<Ui>((set) => ({
  view: "status",
  setView: (view) => set({ view }),
  resumeJob: null,
  openResumes: (jobTextId) => set({ view: "resumes", resumeJob: jobTextId }),
}));
