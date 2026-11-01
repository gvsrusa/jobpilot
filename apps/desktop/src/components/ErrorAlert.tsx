import { Alert, AlertDescription } from "@/components/ui/alert";
import { toAppError } from "@/lib/ipc";

/** `role="alert"` with the stable error code and the core message. */
export function ErrorAlert({ error }: { error: unknown }) {
  if (!error) return null;
  const { code, message } = toAppError(error);
  return (
    <Alert variant="destructive">
      <AlertDescription className="flex justify-between gap-4">
        <span>{message}</span>
        <span className="shrink-0 font-mono text-xs">code: {code}</span>
      </AlertDescription>
    </Alert>
  );
}
