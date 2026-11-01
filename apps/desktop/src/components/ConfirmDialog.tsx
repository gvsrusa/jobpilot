import { useRef, type ReactNode } from "react";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { ErrorAlert } from "./ErrorAlert";

type Props = {
  title: string;
  children: ReactNode;
  confirmLabel: string;
  busyLabel: string;
  onConfirm: () => void;
  onCancel: () => void;
  pending: boolean;
  error?: unknown;
  confirmDisabled?: boolean;
  /** Destructive by default; a warning that only needs a yes uses `default`. */
  confirmVariant?: "destructive" | "default";
};

/**
 * Focus starts on Cancel. While `pending` it cannot be dismissed: restore and delete can
 * wait on a keychain prompt, so the dialog says it is working instead of assuming speed.
 * The body sits in a form, so Enter in a field activates the confirm button only when
 * that button is enabled.
 */
export function ConfirmDialog(props: Props) {
  const { pending, confirmDisabled } = props;
  const cancel = useRef<HTMLButtonElement>(null);
  return (
    <Dialog open onOpenChange={(open) => !open && !pending && props.onCancel()}>
      <DialogContent
        showCloseButton={false}
        onOpenAutoFocus={(event) => {
          event.preventDefault();
          cancel.current?.focus();
        }}
      >
        <form
          className="grid gap-4"
          onSubmit={(event) => {
            event.preventDefault();
            if (!pending && !confirmDisabled) props.onConfirm();
          }}
        >
          <DialogHeader>
            <DialogTitle>{props.title}</DialogTitle>
            <DialogDescription asChild>
              <div className="space-y-2">{props.children}</div>
            </DialogDescription>
          </DialogHeader>
          {pending && (
            <p role="status" className="text-sm text-muted-foreground">
              Working. This can wait on a system keychain prompt, so it may take a while.
              Please keep the app open.
            </p>
          )}
          <ErrorAlert error={props.error} />
          <DialogFooter>
            <Button
              ref={cancel}
              type="button"
              variant="outline"
              disabled={pending}
              onClick={props.onCancel}
            >
              Cancel
            </Button>
            <Button type="submit" variant={props.confirmVariant ?? "destructive"} disabled={pending || confirmDisabled}>
              {pending ? props.busyLabel : props.confirmLabel}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
