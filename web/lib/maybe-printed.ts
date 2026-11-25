// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
import type { Job } from "./api";

/**
 * What a person has to know before sending this job again, or null when the job's end is certain.
 * The server only sends `maybe_printed` on a failed or cancelled job (docs/api.md, "Did it print?").
 */
export function maybePrintedNote(job: Pick<Job, "state" | "agent_name" | "maybe_printed">): string | null {
  if (!job.maybe_printed) return null;
  const look = `${job.agent_name} took this job and never reported back. Check the paper before you send it again.`;
  return job.state === "failed" ? `${look} If ${job.agent_name} reports later, the job turns Done.` : look;
}
