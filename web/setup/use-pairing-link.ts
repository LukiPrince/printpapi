// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
"use client";

import { useEffect, useRef } from "react";
import { setup, type PairingView } from "./api";

/**
 * While mounted and enabled, asks the shell once a second whether a pairing link has arrived (the
 * system hands a `printpapi://` link to this program as its argument, or, on macOS, while it runs)
 * and calls back with it. The shell gives a link out once.
 */
// ponytail: a poll of an in-process value instead of a Tauri event, so the window needs no event
// permission in capabilities/default.json. Ceiling: a second of delay.
export function usePairingLink(onLink: (link: PairingView) => void, enabled = true) {
  const latest = useRef(onLink);
  useEffect(() => {
    latest.current = onLink;
  });
  useEffect(() => {
    if (!enabled) return;
    let live = true;
    const look = () => {
      setup.takeLink().then(
        (link) => {
          if (live && link) latest.current(link);
        },
        () => undefined,
      );
    };
    look();
    const timer = setInterval(look, 1000);
    return () => {
      live = false;
      clearInterval(timer);
    };
  }, [enabled]);
}
