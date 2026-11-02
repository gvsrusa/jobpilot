import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { ipc, type PauseState } from "./ipc";

/** Ctrl+Shift+H, or Cmd+Shift+H on a Mac. The menu accelerator in Rust is the same key. */
export const isPauseKey = (e: KeyboardEvent): boolean =>
  (e.ctrlKey || e.metaKey) && e.shiftKey && e.key.toLowerCase() === "h";

/**
 * The pause, polled every two seconds so a `jobpilot pause set` in a terminal shows up.
 * One owner (`App`) calls this and passes it down, so a failed shortcut and a failed click
 * report through the same alert. Setting and clearing refresh the run list, because pausing
 * halts runs.
 */
export function usePause() {
  const client = useQueryClient();
  const status = useQuery({
    queryKey: ["pause"],
    queryFn: ipc.pauseStatus,
    refetchInterval: 2000,
    retry: false,
  });
  const done = (state: PauseState) => {
    client.setQueryData(["pause"], state);
    void client.resetQueries({ queryKey: ["agents"] });
  };
  const set = useMutation({ mutationFn: ipc.pauseSet, onSuccess: done });
  const clear = useMutation({ mutationFn: ipc.pauseClear, onSuccess: done });
  return { status, set, clear };
}

export type Pause = ReturnType<typeof usePause>;
