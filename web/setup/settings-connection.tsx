// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
"use client";

import { Plug } from "lucide-react";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import type { AgentStatus } from "./api";
import { Actions } from "./bits";

/**
 * Another server or another key. The server ties a device name and its key together and cannot
 * delete devices, so this is a new setup of the agent, started from what is known now.
 */
export function ConnectionSection({ status, onChange }: { status: AgentStatus | null; onChange: () => void }) {
  return (
    <Card>
      <CardHeader>
        <CardTitle>Connection</CardTitle>
        <CardDescription>
          Another server: the device is created there under the same name. A new key on the same
          server needs a new device name.
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-3">
        <dl className="grid grid-cols-[auto_minmax(0,1fr)] gap-x-4 gap-y-1 text-sm">
          <dt className="text-muted-foreground">Server</dt>
          <dd className="truncate font-mono">{status?.server || "—"}</dd>
          <dt className="text-muted-foreground">Device</dt>
          <dd className="truncate">{status?.name || "—"}</dd>
        </dl>
        <p className="text-sm text-muted-foreground">
          The printers carry over and are shown in the next steps. The timeout and the headers under
          Advanced start from their defaults on a new connection.
        </p>
        <Actions>
          <Button type="button" variant="brand" onClick={onChange}>
            <Plug />
            Change server or key
          </Button>
        </Actions>
      </CardContent>
    </Card>
  );
}
