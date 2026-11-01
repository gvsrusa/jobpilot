import { useId, useState } from "react";
import { X } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";

type Props = {
  label: string;
  addLabel: string;
  values: string[];
  placeholder?: string;
  disabled?: boolean;
  /** Returns a message to show under the field, or null when the chip was taken. */
  onAdd(text: string): string | null;
  onRemove(index: number): void;
};

/**
 * Enter on Add opens the input, Enter commits a chip, Backspace on an empty input removes
 * the last chip, Esc closes the input (wireframe screen 7).
 */
export function ChipField(props: Props) {
  const { label, addLabel, values, disabled } = props;
  const errorId = useId();
  const [adding, setAdding] = useState(false);
  const [text, setText] = useState("");
  const [problem, setProblem] = useState<string | null>(null);
  return (
    <div role="group" aria-label={label} className="grid grid-cols-[10rem_1fr] items-start gap-x-4">
      <span className="pt-1.5 text-sm text-muted-foreground">{label}</span>
      <div className="grid gap-1">
        <div className="flex flex-wrap items-center gap-2">
          {values.map((value, index) => (
            <span
              key={`${value}-${index}`}
              className="inline-flex items-center gap-1 rounded-full border px-2 py-0.5 text-sm"
            >
              {value}
              <button
                type="button"
                aria-label={`Remove ${value}`}
                disabled={disabled}
                className="rounded-full p-0.5 hover:bg-muted"
                onClick={() => props.onRemove(index)}
              >
                <X className="size-3" aria-hidden />
              </button>
            </span>
          ))}
          {adding ? (
            <Input
              autoFocus
              className="h-7 w-48"
              aria-label={addLabel}
              aria-describedby={problem ? errorId : undefined}
              aria-invalid={problem ? true : undefined}
              placeholder={props.placeholder}
              value={text}
              onChange={(e) => setText(e.target.value)}
              onBlur={() => {
                if (text.trim() === "") setAdding(false);
              }}
              onKeyDown={(e) => {
                if (e.key === "Enter") {
                  e.preventDefault();
                  const message = text.trim() === "" ? null : props.onAdd(text.trim());
                  setProblem(message);
                  if (!message) setText("");
                } else if (e.key === "Escape") {
                  e.stopPropagation();
                  setAdding(false);
                  setProblem(null);
                } else if (e.key === "Backspace" && text === "" && values.length > 0) {
                  props.onRemove(values.length - 1);
                }
              }}
            />
          ) : (
            <Button
              type="button"
              variant="outline"
              size="sm"
              disabled={disabled}
              aria-label={addLabel}
              onClick={() => setAdding(true)}
            >
              Add
            </Button>
          )}
        </div>
        {problem && (
          <p id={errorId} className="text-xs text-destructive">
            {problem}
          </p>
        )}
      </div>
    </div>
  );
}
