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
          {status?.keyUnreadable
            ? "The key stored on this computer cannot be read. Create a code with Reconnect on this device in your dashboard and enter it here: the device keeps its name and printers. Or enter the key this device already uses."
            : "Another server: the device is created there. The same server: create a code with Reconnect on this device in the dashboard; it keeps its name and printers."}
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
          The printers carry over and are shown in the next steps. The timeout carries over. Headers
          under Advanced carry over with a typed address and key; a pairing link to another server
          removes them, so that server never sees them.
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
