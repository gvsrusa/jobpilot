import { useState } from "react";
import { Button } from "@/components/ui/button";

export function CopyPath({ path }: { path: string }) {
  const [copied, setCopied] = useState(false);
  return (
    <Button
      variant="outline"
      size="sm"
      onClick={() => {
        void navigator.clipboard.writeText(path).then(() => setCopied(true));
      }}
    >
      {copied ? "Copied" : "Copy path"}
    </Button>
  );
}
