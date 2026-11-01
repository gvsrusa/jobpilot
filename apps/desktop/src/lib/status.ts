import { useQuery, useQueryClient, type QueryClient } from "@tanstack/react-query";
import { ipc, type Status } from "./ipc";

export const useStatus = () => useQuery({ queryKey: ["status"], queryFn: ipc.status });

/**
 * Every command that answers with a `Status` goes through here. `Ok` does not mean it
 * worked: a "recovery" status makes the app show the recovery view with its error.
 * Restore, delete and retry can change all data, so other cached data is dropped.
 */
export function applyStatus(client: QueryClient, status: Status) {
  client.setQueryData(["status"], status);
  // Reset, not just invalidate: after a wipe or restore, cached rows must not show.
  void client.resetQueries({ predicate: (q) => q.queryKey[0] !== "status" });
}

export const useApplyStatus = () => {
  const client = useQueryClient();
  return (status: Status) => applyStatus(client, status);
};
