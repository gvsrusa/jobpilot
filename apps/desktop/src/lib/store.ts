import { create } from "zustand";

export type View = "status" | "documents" | "data" | "profile" | "resumes" | "ai" | "applications" | "memory" | "agents" | "jobs";

export const useUiStore = create<{ view: View; setView(v: View): void }>((set) => ({
  view: "status",
  setView: (view) => set({ view }),
}));
