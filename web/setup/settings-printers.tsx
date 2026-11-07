// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
"use client";

import { Pencil } from "lucide-react";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import type { AgentStatus } from "./api";
import { Actions, Notice } from "./bits";

/** The printers the agent reports. Changing them is the printer step of the setup, applied as a change. */
export function PrintersSection({ status, onChange }: { status: AgentStatus | null; onChange: () => void }) {
  const known = status !== null && status.name !== "";
  return (
    <Card>
      <CardHeader>
        <CardTitle>Printers</CardTitle>
        <CardDescription>
          Add or remove printers, change their kind, print a test. You will be asked for administrator
          rights once. A newer setup file updates the agent when you apply a change.
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-3">
        {!known ? (
          <Notice tone="warn">
            The agent&apos;s printers are not known while it is not running. Use Restart first.
          </Notice>
        ) : (
          <ul className="space-y-1.5">
            {status.printers.map((p) => (
              <li key={p.name} className="flex flex-wrap items-center gap-2 rounded-lg border px-3 py-2 text-sm">
                <span className="min-w-0 flex-1 truncate font-medium" title={p.name}>
                  {p.name}
                </span>
                {p.locked && <Badge variant="secondary">set by hand</Badge>}
                {p.target?.startsWith("socket://") && <Badge variant="secondary">network</Badge>}
                <Badge variant="outline">{p.pdf ? "PDF" : "raw"}</Badge>
              </li>
            ))}
          </ul>
        )}
        <Actions>
          <Button type="button" variant="brand" disabled={!known} onClick={onChange}>
            <Pencil />
            Change printers
          </Button>
        </Actions>
      </CardContent>
    </Card>
  );
}
