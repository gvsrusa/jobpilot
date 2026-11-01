import { useQuery, useQueryClient, type QueryClient } from "@tanstack/react-query";
import { ipc, type Status } from "./ipc";

export const useStatus = () => useQuery({ queryKey: ["status"], queryFn: ipc.status });

/**
 * Every command that answers with a `Status` goes through here. `Ok` does not mean it
 * worked: a "recovery" status makes the app show the recovery view with its error.
 * Restore, delete and retry can change all data, so everything is refetched.
 */
export function applyStatus(client: QueryClient, status: Status) {
  client.setQueryData(["status"], status);
  void client.invalidateQueries();
}

export const useApplyStatus = () => {
  const client = useQueryClient();
  return (status: Status) => applyStatus(client, status);
};
