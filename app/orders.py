# printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
"""Store order payloads -> the normalized order dict `packing_slip.render_packing_slip` prints.

Pure logic, no IO — the mapping is the part that differs per shop system, and the part most
likely to need fixing when a shop sends something odd, so it stays testable on its own.

Amounts are passed through as the shop wrote them (the shop already did the arithmetic). The one
exception is a Shopify line amount: Shopify only sends a unit price, so qty x price is computed
in Decimal here.
"""
from decimal import Decimal, InvalidOperation

MAX_LINES = 1000          # a packing slip, not a warehouse report — also caps the PDF size
_MAX_FIELD = 200          # per-field character cap; the slip truncates to the column anyway
_LINE_KEYS = ("qty", "sku", "name", "total")
_ORDER_KEYS = ("number", "date", "shop", "customer", "email", "phone", "note", "source_id")


class OrderError(ValueError):
    """A payload we refuse to turn into a packing slip (client's fault → 400)."""


def _s(v):
    return "" if v is None else str(v).strip()[:_MAX_FIELD]


def _money(value, currency=""):
    """'19.80' -> '19.80 EUR'. Empty/zero/unparseable -> None, so no '0.00' rows get printed."""
    text = _s(value)
    if not text:
        return None
    try:
        if Decimal(text) == 0:
            return None
    except InvalidOperation:
        pass                                  # not a number we understand — print it verbatim
    return f"{text} {currency}".strip()


def _address(parts, city_line, country):
    return [p for p in (*parts, city_line, country) if p]


def _from_shopify(p):
    ship = p.get("shipping_address") or {}
    cust = p.get("customer") or {}
    name = _s(ship.get("name")) or _s(f"{_s(cust.get('first_name'))} {_s(cust.get('last_name'))}")
    currency = _s(p.get("currency"))
    city_line = " ".join(x for x in (_s(ship.get("zip")), _s(ship.get("city"))) if x)
    province = _s(ship.get("province"))
    if province and province != _s(ship.get("city")):
        city_line = f"{city_line}, {province}".lstrip(", ")

    lines = []
    for it in p.get("line_items") or []:
        title = _s(it.get("title"))
        variant = _s(it.get("variant_title"))
        if variant and variant != "Default Title":
            title = f"{title} ({variant})"
        lines.append({"qty": it.get("quantity"), "sku": it.get("sku"), "name": title,
                      "total": _money(_line_total(it.get("price"), it.get("quantity")), currency)})

    shipping = sum((_dec(s.get("price")) for s in p.get("shipping_lines") or []), Decimal(0))
    totals = [("Subtotal", p.get("subtotal_price")), ("Shipping", shipping),
              ("Tax", p.get("total_tax")), ("Total", p.get("total_price"))]
    return {
        "number": _s(p.get("name")) or f"#{_s(p.get('order_number'))}",
        "date": _s(p.get("created_at"))[:10],
        "customer": name,
        "address": _address((_s(ship.get("address1")), _s(ship.get("address2"))),
                            city_line, _s(ship.get("country"))),
        "email": _s(p.get("email")) or _s(cust.get("email")),
        "phone": _s(ship.get("phone")) or _s(cust.get("phone")),
        "note": _s(p.get("note")),
        "source_id": _s(p.get("id")),
        "lines": lines,
        "totals": [[label, _money(v, currency)] for label, v in totals],
    }


def _from_woocommerce(p):
    ship = p.get("shipping") or {}
    bill = p.get("billing") or {}
    who = ship if _s(ship.get("first_name")) or _s(ship.get("last_name")) else bill
    currency = _s(p.get("currency"))
    city_line = " ".join(x for x in (_s(who.get("postcode")), _s(who.get("city"))) if x)
    state = _s(who.get("state"))
    if state and state != _s(who.get("city")):
        city_line = f"{city_line}, {state}".lstrip(", ")

    totals = [("Discount", p.get("discount_total")), ("Shipping", p.get("shipping_total")),
              ("Tax", p.get("total_tax")), ("Total", p.get("total"))]
    return {
        "number": _s(p.get("number")) or _s(p.get("id")),
        "date": _s(p.get("date_created"))[:10],
        "customer": _s(f"{_s(who.get('first_name'))} {_s(who.get('last_name'))}"),
        "address": _address((_s(who.get("address_1")), _s(who.get("address_2"))),
                            city_line, _s(who.get("country"))),
        "email": _s(bill.get("email")),
        "phone": _s(bill.get("phone")),
        "note": _s(p.get("customer_note")),
        "source_id": _s(p.get("id")),
        "lines": [{"qty": it.get("quantity"), "sku": it.get("sku"), "name": it.get("name"),
                   "total": _money(it.get("total"), currency)}
                  for it in p.get("line_items") or []],
        "totals": [[label, _money(v, currency)] for label, v in totals],
    }


def _dec(v):
    try:
        return Decimal(_s(v) or "0")
    except InvalidOperation:
        return Decimal(0)


def _line_total(price, qty):
    try:
        return f"{_dec(price) * int(qty or 0):.2f}"
    except (ValueError, TypeError):
        return _s(price)


_FORMATS = {"shopify": _from_shopify, "woocommerce": _from_woocommerce}


def normalize_order(payload, fmt=None):
    """Map a store payload (`fmt`) — or validate an already-normalized order (`fmt` None) —
    into the dict the packing slip renders. Raises OrderError on anything unprintable."""
    if fmt is not None:
        if fmt not in _FORMATS:
            raise OrderError(f"unknown order format: {fmt!r} "
                             f"(expected one of {', '.join(sorted(_FORMATS))})")
        if not isinstance(payload, dict):
            raise OrderError("order must be an object")
        payload = _FORMATS[fmt](payload)

    if not isinstance(payload, dict):
        raise OrderError("order must be an object")
    out = {k: _s(payload.get(k)) for k in _ORDER_KEYS if _s(payload.get(k))}
    if not out.get("number"):
        raise OrderError("order needs a 'number'")

    address = payload.get("address")
    if address is not None and not isinstance(address, list):
        raise OrderError("'address' must be a list of lines")
    out["address"] = [_s(a) for a in (address or [])[:10] if _s(a)]

    lines = payload.get("lines")
    if lines is not None and not isinstance(lines, list):
        raise OrderError("'lines' must be a list")
    lines = lines or []
    if len(lines) > MAX_LINES:
        raise OrderError(f"too many line items ({len(lines)}, max {MAX_LINES})")
    out["lines"] = [{k: _s(it.get(k)) for k in _LINE_KEYS if _s(it.get(k))}
                    for it in lines if isinstance(it, dict)]

    totals = payload.get("totals")
    if totals is not None and not isinstance(totals, list):
        raise OrderError("'totals' must be a list of [label, amount] pairs")
    out["totals"] = [[_s(t[0]), _s(t[1])] for t in (totals or [])[:20]
                     if isinstance(t, (list, tuple)) and len(t) == 2 and _s(t[1])]
    return out
