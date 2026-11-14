// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
import test from "node:test";
import assert from "node:assert/strict";
import {
  activeGrant,
  grantMinutes,
  grantOutcome,
  idsSummary,
  showSupportCard,
  type SupportGrant,
} from "./support-access.ts";

const grant = (over: Partial<SupportGrant> = {}): SupportGrant => ({
  id: 1,
  reason: "ticket 4711",
  granted_at: 1_000,
  expires_at: 1_000 + 30 * 60,
  ended_at: null,
  active: false,
  listed_job_ids: [],
  opened_job_ids: [],
  ...over,
});

test("grantMinutes is the granted window, not the time actually used", () => {
  assert.equal(grantMinutes(grant()), 30);
  assert.equal(grantMinutes(grant({ expires_at: 1_000 + 15 * 60, ended_at: 1_100 })), 15);
});

test("grantOutcome tells active, ended early and expired apart", () => {
  assert.equal(grantOutcome(grant({ active: true })), "active");
  assert.equal(grantOutcome(grant({ ended_at: 1_200 })), "ended early");
  assert.equal(grantOutcome(grant()), "expired");
});

test("activeGrant finds the one active grant or null", () => {
  assert.equal(activeGrant([grant(), grant({ id: 2 })]), null);
  assert.equal(activeGrant([grant(), grant({ id: 2, active: true })])?.id, 2);
});

test("idsSummary lists a few ids, then counts the rest", () => {
  assert.equal(idsSummary([]), "none");
  assert.equal(idsSummary([7]), "#7");
  assert.equal(idsSummary([1, 2, 3]), "#1, #2, #3");
  assert.equal(idsSummary([1, 2, 3, 4, 5, 6, 7]), "#1, #2, #3, #4, #5 and 2 more");
});

test("the card shows only when enabled or there is history", () => {
  assert.equal(showSupportCard({ enabled: false, grants: [] }), false);
  assert.equal(showSupportCard({ enabled: true, grants: [] }), true);
  assert.equal(showSupportCard({ enabled: false, grants: [grant()] }), true);
});
