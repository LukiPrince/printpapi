// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
// The setup files of a release, for the downloads page. The list itself (setup-downloads.json) is
// shared with .github/workflows/release.yml, which builds and labels exactly these files.
import list from "./setup-downloads.json" with { type: "json" };

const LATEST = "https://github.com/LukiPrince/printpapi/releases/latest/download/";

export type SetupDownload = {
  platform: "windows" | "macos" | "linux";
  label: string;
  url: string;
  beta: boolean;
};

export function setupDownloads(): SetupDownload[] {
  return list.map((d) => ({
    platform: d.platform as SetupDownload["platform"],
    label: d.label,
    url: LATEST + d.asset,
    beta: d.beta,
  }));
}
