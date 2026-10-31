"""Regenerate the golden files the Rust packing slip / order tests compare against.

Runs the ORIGINAL Python implementation (the repo's `app/` package, as of the last Python
release) and writes, next to this script:
  <case>.json      the normalized order dict that was rendered
  <case>.pdf       render_packing_slip(<that dict>) byte for byte
  <case>.raw.json  (payload cases only) {"format": ..., "payload": ...} fed to normalize_order first
  orders_cases.json  normalize_order edge cases: {"format", "payload", "expected" | "error"}
  n0_cases.json    [[repr(x), f"{x:.0f}"], ...] for the coordinate rounding
Usage (from anywhere): python make_golden.py
"""
import json
import random
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parents[4]))          # the repo root, so `import app` resolves

from app.orders import OrderError, normalize_order  # noqa: E402
from app.packing_slip import render_packing_slip  # noqa: E402


def dump(path, obj):
    path.write_text(json.dumps(obj, ensure_ascii=False, indent=1) + "\n", encoding="utf-8")


def line(i, **kw):
    return dict({"qty": i % 5 + 1, "sku": f"S-{i}", "name": f"Item {i}",
                 "total": f"{i}.{i % 100:02d} EUR"}, **kw)


SHOPIFY = {
    "id": 450789469, "order_number": 1001, "name": "#1001",
    "created_at": "2026-07-25T10:22:51-04:00", "currency": "EUR",
    "subtotal_price": "29.70", "total_tax": "0.00", "total_price": "34.60",
    "note": "leave at the door", "email": "jane@example.com",
    "customer": {"first_name": "Jane", "last_name": "Doe"},
    "shipping_address": {"name": "Jane Doe", "address1": "Musterweg 1", "address2": "",
                         "zip": "12345", "city": "Berlin", "province": "Brandenburg",
                         "country": "Germany", "phone": "+49 30 123456"},
    "shipping_lines": [{"title": "Standard", "price": "4.90"}, {"title": "Fast", "price": "2.5"}],
    "line_items": [{"quantity": 2, "sku": "A-1", "title": "Widget", "variant_title": "blue",
                    "price": "9.90"},
                   {"quantity": 1, "sku": "B-2", "title": "Gadget",
                    "variant_title": "Default Title", "price": "9.90"},
                   {"quantity": 3, "sku": "C-3", "title": "Ümlaut “quoted” (x)", "price": "0.335"}],
}

WOO = {
    "id": 727, "number": "727", "date_created": "2026-07-25T10:22:51", "currency": "EUR",
    "total": "34.60", "shipping_total": "4.90", "total_tax": "3.20", "discount_total": "1.00",
    "customer_note": "leave at the door",
    "billing": {"email": "jane@example.com", "phone": "+49 30 123456"},
    "shipping": {"first_name": "Jane", "last_name": "Doe", "address_1": "Musterweg 1",
                 "address_2": "Hinterhaus", "postcode": "12345", "city": "Berlin", "state": "BE",
                 "country": "DE"},
    "line_items": [{"name": "Widget", "quantity": 2, "sku": "A-1", "total": "19.80"},
                   {"name": "Gadget", "quantity": 1, "sku": "B-2", "total": "9.90"}],
}

DIRECT = {
    "minimal": {"number": "7"},
    "full": {
        "number": "1001", "date": "2026-07-25", "shop": "Acme Store", "customer": "Jane Doe",
        "address": ["Musterweg 1", "12345 Berlin", "DE"], "email": "jane@example.com",
        "phone": "+49 30 123456",
        "lines": [{"qty": 2, "sku": "A-1", "name": "Widget", "total": "19.80 EUR"},
                  {"qty": 1, "sku": "B-2", "name": "Gadget (large)", "total": "9.90 EUR"},
                  {"qty": 1, "name": "No sku, no total"}],
        "totals": [["Subtotal", "29.70 EUR"], ["Shipping", "4.90 EUR"], ["Total", "34.60 EUR"]],
        "note": "leave at the door", "source_id": "42",
    },
    "multi_page": {"number": "2000", "customer": "Bulk Buyer", "shop": "Big Shop",
                   "address": ["Lagerstr. 9", "10115 Berlin"],
                   "lines": [line(i) for i in range(150)],
                   "totals": [["Subtotal", "1.00"], ["Tax", "0.19"], ["Total", "1.19"]],
                   "note": "pallet"},
    "long_names": {"number": "3" * 40, "customer": "N" * 150, "shop": "S" * 60,
                   "address": ["A" * 130, "B" * 99, "C"], "email": "e" * 120 + "@example.com",
                   "lines": [{"qty": "1" * 20, "sku": "SKU-" + "X" * 60, "name": "Name " + "y" * 150,
                              "total": "123456789.99 EUR"},
                             {"qty": 1, "sku": "short", "name": "w" * 51, "total": "1"},
                             {"qty": 1, "sku": "z" * 12, "name": "v" * 50, "total": "2"}],
                   "totals": [["A very long total label that overflows", "999999.99 EUR"]],
                   "note": "n" * 190},
    "non_latin1": {"number": "9 €", "shop": "Café Zürich ß", "customer": "Jörg Müller 日本語",
                   "address": ["Straße 1 😀", "Ĉapelo ŝ", "\u0080\u0081\u009f ‘x’ “y” — … ™ Ÿ"],
                   "lines": [{"qty": 1, "sku": "€-1", "name": "Größe XL — 日本 😀 ñ", "total": "5 €"},
                             {"qty": 1, "name": "œ Œ ž Ž š Š ƒ † ‡ ‰ • ˜ ˆ", "total": "6"}],
                   "totals": [["Gesamt €", "11 €"]], "note": "Grüße 日本"},
    "parens_backslashes": {"number": "(1)", "customer": r"Foo (Bar) \ Baz",
                           "address": ["C:\\dir\\file", "((nested))", "a\\(b\\)"],
                           "lines": [{"qty": 1, "sku": "\\(", "name": "x) y ( z \\\\", "total": "(1)"}],
                           "totals": [["Sub(total)", "1\\2"]], "note": "back\\slash (paren)"},
    "wide_amounts": {"number": "55", "date": "2026-01-01", "shop": "W",
                     "lines": [line(i, total="1" * (i % 30 + 1), name="n" * (i % 70 + 1))
                               for i in range(60)],
                     "totals": [["T" * i, "9" * i] for i in range(1, 12)]},
}


def force_totals_break():
    """Pick a line count where only the totals block spills onto a new page."""
    base = {"number": "88", "customer": "Edge Case"}
    tail = {"totals": [["Subtotal", "1"], ["Tax", "2"], ["Total", "3"]], "note": "last words"}
    for n in range(20, 80):
        lines = [line(i) for i in range(n)]
        a = render_packing_slip(normalize_order(dict(base, lines=lines), None))
        b = render_packing_slip(normalize_order(dict(base, lines=lines, **tail), None))
        if a.count(b"/Type /Page ") < b.count(b"/Type /Page "):
            return dict(base, lines=lines, **tail)
    raise SystemExit("no line count forces a totals-only page break")


DIRECT["totals_page_break"] = force_totals_break()
RAW = {"shopify": ("shopify", SHOPIFY), "woocommerce": ("woocommerce", WOO)}

# normalize_order edge cases, shaped by what shops (and bots) actually send
ODD = [
    ("shopify", {"id": 1}),
    ("shopify", {"name": "  #5  ", "line_items": [{"quantity": "3", "price": "1.115", "title": "T"}]}),
    ("shopify", {"id": 2, "line_items": [{"quantity": 2.9, "price": 9.5, "title": "F"}]}),
    ("shopify", {"id": 2, "line_items": [{"quantity": True, "price": "5"}, {"quantity": None, "price": "5"},
                                         {"quantity": "x", "price": "5.5"}, {"quantity": "2.5", "price": "1"},
                                         {"quantity": " 4 ", "price": "-1.005"}, {"quantity": [1], "price": "7"},
                                         {"price": "abc", "quantity": 2}, {"quantity": "1_0", "price": "1"}]}),
    ("shopify", {"id": 3, "currency": "", "subtotal_price": "5", "total_price": 6.5,
                 "shipping_lines": [{"price": "0.0000001"}], "total_tax": "1e3"}),
    ("shopify", {"id": 4, "shipping_lines": [{"price": "4.9"}, {"price": "x"}, {"price": None}, {"price": 3}]}),
    ("shopify", {"id": 5, "shipping_lines": [{"price": "NaN"}], "total_tax": "NaN", "total_price": "Infinity"}),
    ("shopify", {"id": 6, "currency": "EUR", "total_price": "free", "subtotal_price": "-0.00", "total_tax": " 7 "}),
    ("shopify", {"id": 7, "shipping_address": {"city": "Köln", "province": "Köln", "zip": "5"}}),
    ("shopify", {"id": 8, "shipping_address": {"province": "Bayern"}, "customer": {"first_name": "A"}}),
    ("shopify", {"id": 9, "customer": {"email": "c@x", "phone": "1", "last_name": "Z"}}),
    ("shopify", {"id": 10, "created_at": "2026-07-25T10", "note": "x" * 300, "email": "  "}),
    ("shopify", {"order_number": 12, "name": ""}),
    ("shopify", {"id": 11, "line_items": [{"title": "T", "variant_title": "v", "sku": 5, "quantity": 1.0}]}),
    ("shopify", {"id": 12, "line_items": [{"title": "", "variant_title": "v", "price": "2", "quantity": 1}]}),
    ("woocommerce", {"id": 1}),
    ("woocommerce", {"id": 2, "number": 5, "billing": {"first_name": "B", "city": "X", "state": "Y",
                                                       "email": "e", "phone": 3}, "shipping": {"city": "Z"}}),
    ("woocommerce", {"id": 3, "currency": "USD", "total": 12, "shipping_total": 0, "total_tax": 0.0,
                     "discount_total": "-5.00",
                     "line_items": [{"name": None, "quantity": 0, "sku": "", "total": "0.00"},
                                    {"name": 5, "quantity": "2", "sku": None, "total": 3.5}]}),
    ("woocommerce", {"id": 4, "shipping": {"last_name": "Only"}, "billing": {"first_name": "No"}}),
    ("woocommerce", {"number": "", "id": None}),
    (None, {"number": " 1 ", "date": 5, "address": ["a", "", None, "b"] + ["c"] * 12,
            "lines": [{"qty": 1.5, "sku": True, "name": None, "total": []}, 7, [], {}],
            "totals": [["a", "b"], ["c", ""], ["d"], "xy", [1, 2], ["e", None], ["f", 0]]}),
    (None, {"number": "1", "address": None, "lines": None, "totals": None}),
    (None, {"number": "1", "address": "x"}),
    (None, {"number": "1", "address": 0}),
    (None, {"number": "1", "lines": ""}),
    (None, {"number": "1", "lines": [{}] * 1000}),
    (None, {"number": "1", "lines": [{}] * 1001}),
    (None, {"number": "1", "totals": {}}),
    (None, {"number": "1", "totals": "ab"}),
    (None, {"number": "\u00a0\u001f x \u2003"}),
    (None, {"number": "   "}),
    (None, {"number": None}),
    (None, {"number": False}),
    (None, {"number": 0}),
    (None, {"number": 1.0, "note": 1e22, "phone": 2.5e-7, "shop": {"a": [1, "b'c", None]}}),
    (None, [1, 2]),
    (None, "str"),
    ("", {"id": 1}),
    (5, {"id": 1}),
    ("Shopify", {"id": 1}),
    ("shopify", [1]),
    ("shopify", "str"),
    ("woocommerce", None),
]


def build_orders_cases():
    cases = []
    for fmt, payload in ODD:
        case = {"format": fmt, "payload": payload}
        try:
            case["expected"] = normalize_order(payload, fmt)
        except OrderError as exc:
            case["error"] = str(exc)
        except Exception:
            continue                       # the Python crashed here: no behavior to compare
        cases.append(case)
    return cases


def build_n0_cases():
    rng = random.Random(1234)
    xs = [i / 2 for i in range(-4, 2400)]
    xs += [555 - n * 0.58 * size for n in range(0, 400) for size in (8, 10, 12, 18)]
    xs += [rng.uniform(-50, 600) for _ in range(3000)]
    xs += [rng.randrange(0, 6000) / 10 + 0.05 * rng.randrange(0, 3) for _ in range(1000)]
    return [[repr(x), f"{x:.0f}"] for x in xs]


def main():
    for name, order in DIRECT.items():
        norm = normalize_order(order, None)
        dump(HERE / f"{name}.json", norm)
        (HERE / f"{name}.pdf").write_bytes(render_packing_slip(norm))
    for name, (fmt, payload) in RAW.items():
        norm = normalize_order(payload, fmt)
        dump(HERE / f"{name}.raw.json", {"format": fmt, "payload": payload})
        dump(HERE / f"{name}.json", norm)
        (HERE / f"{name}.pdf").write_bytes(render_packing_slip(norm))
    dump(HERE / "orders_cases.json", build_orders_cases())
    dump(HERE / "n0_cases.json", build_n0_cases())


if __name__ == "__main__":
    main()
