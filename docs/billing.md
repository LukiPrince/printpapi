# Billing (plans, checkout links, one signed webhook)

For a **hosted** printpapi that charges money. A self-hosted box configures none of this, and
every billing route stays off (`503`, and `GET /plans` returns an empty catalogue).

printpapi does not integrate a payment provider — it takes a **plan catalogue** and a **signed
callback**. Every provider (Stripe, Paddle, Lemon Squeezy, an invoice you send by hand) can
produce a hosted checkout link and call a webhook, so that is the whole contract: no SDK, no
dependency, and no card data ever touches this server.

The plan is bookkeeping. Its `jobs` is what actually bites, because it is written straight into
the org's [`job_quota`](api.md#quotas), which the enqueue path already enforces on every submit
route. One number, one guard.

## 1. Write the catalogue

`PRINTAPI_PLANS` is either the JSON array itself or a path to a file holding one (so a compose
file can mount it):

```json
[
  { "id": "free", "name": "Free", "jobs": 50, "price": "0 €" },
  { "id": "pro",  "name": "Pro",  "jobs": 5000, "price": "9 €/mo",
    "checkout_url": "https://buy.stripe.com/xxxx?client_reference_id={org}" },
  { "id": "unlimited", "name": "Unlimited", "jobs": null, "price": "29 €/mo",
    "checkout_url": "https://buy.stripe.com/yyyy?client_reference_id={org}" }
]
```

- `jobs` — the monthly job quota; `null` is unlimited.
- `price` — free-form text, shown in the dashboard. Nothing computes with it.
- `checkout_url` — the provider's hosted payment link. **`{org}` is substituted with the calling
  org's id**, which is how the provider hands the org back to you in the webhook
  (Stripe `client_reference_id`, Paddle custom data, Lemon Squeezy `checkout[custom][org_id]` —
  all of them echo a query parameter into the event).
- **The first plan is the default**: where an org lands when a subscription ends. Make it the free
  tier (or one with `"jobs": 0` if a lapsed account should print nothing).

`GET /plans` (any credential) returns the catalogue with the links already filled in, plus
`"current"` — the caller's plan. That is what the dashboard's Settings page renders.

## 2. Point the provider at the webhook

```
POST /billing/webhook
X-Signature: sha256=<hex hmac-sha256 of the raw body, keyed with PRINTAPI_BILLING_SECRET>

{"org_id": 7, "plan": "pro", "status": "active"}
```

- The org is named by `org_id` **or** by `email` (the address of any account in it).
- `status` defaults to `active`. `active`, `trialing`, `paid`, `completed` and `succeeded` apply
  the named plan; **anything else** (`cancelled`, `past_due`, `refunded`, …) ignores it and
  downgrades to the default plan — a cancellation event carries the *old* plan id, and honouring
  it would keep the quota the tenant just stopped paying for.
- Answers: `200 {ok, org_id, plan, job_quota}`, `401` bad or missing signature, `400` bad JSON /
  unknown plan / no org named, `404` unknown org, `503` billing not configured.
- Applying the same event twice is a no-op, so a provider's retry is safe.

Providers sign their own way (Stripe's `Stripe-Signature` is `t=…,v1=…` over `timestamp.body`;
Paddle and Lemon Squeezy differ again), and none of them let you author a plain HMAC header. So
put the provider's own webhook through a five-line adapter — a serverless function, an n8n node,
a reverse-proxy script — that verifies *their* signature, maps their event to the body above and
re-signs it with `PRINTAPI_BILLING_SECRET`. That adapter is where a provider's quirks live, and
swapping providers never touches this server.

```python
# the whole adapter, whichever provider you are on
body = json.dumps({"org_id": org, "plan": plan, "status": status}).encode()
sig = hmac.new(SECRET.encode(), body, hashlib.sha256).hexdigest()
requests.post(f"{PRINTPAPI}/billing/webhook", data=body,
              headers={"X-Signature": f"sha256={sig}"})
```

## 3. Moving an org by hand

`PUT /orgs/{id} {"plan": "pro"}` does the same thing with the bootstrap token — for a customer you
invoice manually, or to fix a missed event. **Root only**: a tenant that could set its own plan
would have no cap (same rule as `job_quota`). The dashboard shows those buttons only to root.

## 4. When a customer leaves

`DELETE /orgs/{id}` (root only) removes the org and everything in it — jobs, printers, agents,
keys, accounts and their sessions. There is no undo and no soft delete; it is the "forget me"
button. `DEFAULT_ORG` (id 1) is refused, because an agent presenting an unknown key still enrolls
there.

## Environment

| Variable | Purpose |
|---|---|
| `PRINTAPI_PLANS` | The catalogue: a JSON array, or a path to a file holding one. Unset ⇒ billing off |
| `PRINTAPI_BILLING_SECRET` | Shared secret the webhook signature is checked against. Unset ⇒ billing off |

## Ceilings (deliberate)

- **No provider integration.** No checkout API call, no subscription lookup, no invoice history —
  a link out and a callback in. Add an adapter, not code here.
- **No timestamp in the signature**, so a captured event can be replayed. Replaying re-applies the
  same plan, which is idempotent; a signed timestamp window would only matter if event *order*
  ever mattered.
- **No proration, trials or seats.** A plan is a monthly job cap and a price string.
- **The quota window is the UTC calendar month**, not the subscription's own billing date — see
  [Quotas](api.md#quotas).
- **Nothing dunns.** A `402` is where an over-quota tenant finds out; this server never mails.
