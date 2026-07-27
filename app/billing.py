# printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
"""Billing, provider-agnostic: a plan catalogue the operator writes, and the parsing/verification
for the webhook a payment provider calls when a subscription starts, renews or ends.

Deliberately *not* a payment integration. Every provider (Stripe, Paddle, Lemon Squeezy, an
invoice by hand) can produce a hosted checkout link and an HTTP callback, so that is all this
takes: a link per plan, and one signed POST saying "this org is on that plan now". No SDK, no
dependency, no card data ever touching this server.

The plan is bookkeeping; the plan's `jobs` is what actually bites, because it is written into
`orgs.job_quota`, which `store.enqueue_job` already enforces on every submit path. One number,
one guard — a plan cannot disagree with the quota it grants.

Pure: no IO, no DB, no clock. `app/server.py` does the resolving and storing.
"""
import hashlib
import hmac
import json

# Statuses that mean "this subscription is paid for". Everything else — cancelled, past_due,
# unpaid, refunded, whatever a provider invents — falls back to the default (first) plan.
ACTIVE = ("active", "trialing", "paid", "completed", "succeeded")


class BillingError(ValueError):
    """Bad catalogue or bad event. A ValueError so a caller can catch it with json's."""


def load_plans(raw):
    """Parse the PRINTAPI_PLANS catalogue. Empty/unset returns [] — billing off, self-host default.

    A plan is `{"id", "name"?, "jobs"?, "price"?, "checkout_url"?}`: `jobs` is the monthly job
    quota (null = unlimited), `price` is free-form display text, `checkout_url` is the provider's
    payment link (`{org}` is substituted, see `checkout_url`). The **first plan is the default** —
    what an org lands on when a subscription ends, so make it the free tier.
    """
    raw = (raw or "").strip()
    if not raw:
        return []
    try:
        data = json.loads(raw)
    except ValueError as e:
        raise BillingError(f"plan catalogue is not valid JSON: {e}") from e
    if not isinstance(data, list) or not data:
        raise BillingError("plan catalogue must be a non-empty JSON array")
    plans, seen = [], set()
    for item in data:
        if not isinstance(item, dict):
            raise BillingError("every plan must be a JSON object")
        pid = str(item.get("id") or "").strip()
        if not pid:
            raise BillingError("every plan needs an id")
        if pid in seen:
            raise BillingError(f"duplicate plan id: {pid}")
        jobs = item.get("jobs")
        if jobs is not None and (isinstance(jobs, bool) or not isinstance(jobs, int) or jobs < 0):
            raise BillingError(f"plan {pid}: jobs must be a non-negative integer or null")
        seen.add(pid)
        plans.append({"id": pid, "name": str(item.get("name") or pid), "jobs": jobs,
                      "price": item.get("price"), "checkout_url": item.get("checkout_url")})
    return plans


def find(plans, plan_id):
    pid = str(plan_id or "").strip()
    return next((p for p in plans if p["id"] == pid), None)


def default_plan(plans):
    """Where an org without a live subscription belongs — the catalogue's first entry."""
    return plans[0] if plans else None


def checkout_url(plan, org_id):
    """The plan's payment link with `{org}` filled in, so the provider echoes the org id back in
    its webhook (Stripe `client_reference_id`, Paddle custom data, Lemon Squeezy `checkout[custom]`
    — all of them take a query parameter and hand it back)."""
    url = plan.get("checkout_url")
    return url.replace("{org}", str(org_id)) if url and org_id is not None else url


def verify(secret, raw_body, header):
    """Constant-time check of a hex sha256 HMAC over the *raw* body, with or without the
    `sha256=` prefix providers like to put in front of it.

    # ponytail: one shared secret, no timestamp, so a captured event can be replayed. Replaying
    # only re-applies the same plan, which is idempotent; add a signed timestamp window if a
    # provider ever sends events whose order matters.
    """
    if not secret or not header:
        return False
    got = header.split("=", 1)[1] if header.lower().startswith("sha256=") else header
    want = hmac.new(secret.encode(), raw_body, hashlib.sha256).hexdigest()
    return hmac.compare_digest(got.strip().lower(), want)


def parse_event(body, plans):
    """A provider event -> `{org_id, email, plan, status}` for the server to apply.

    Shape (whatever the provider sends is mapped to this by its own template/adapter — every one
    of them lets you author the callback body, or you put a five-line function in front):

        {"org_id": 7, "plan": "pro", "status": "active"}
        {"email": "owner@shop.example", "plan": "pro"}       # org named by its owner instead

    `status` defaults to active. A non-active status ignores the named plan and downgrades to the
    default one — a cancellation carries the *old* plan id, and honouring it would keep the quota
    the tenant just stopped paying for.
    """
    if not isinstance(body, dict):
        raise BillingError("event must be a JSON object")
    org_id = body.get("org_id")
    if org_id is not None:
        try:                                   # providers echo custom data back as strings
            org_id = int(str(org_id).strip())
        except (TypeError, ValueError):
            raise BillingError("org_id must be an integer") from None
    email = (body.get("email") or "").strip().lower()
    if org_id is None and not email:
        raise BillingError("org_id or email required")
    status = str(body.get("status") or "active").strip().lower()
    if status in ACTIVE:
        plan = find(plans, body.get("plan"))
        if plan is None:
            raise BillingError(f"unknown plan: {body.get('plan')!r} "
                               f"(have: {', '.join(p['id'] for p in plans)})")
    else:
        plan = default_plan(plans)
    return {"org_id": org_id, "email": email, "plan": plan, "status": status}
