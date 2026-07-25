import pytest

from app.orders import OrderError, normalize_order

SHOPIFY = {
    "id": 450789469, "order_number": 1001, "name": "#1001",
    "created_at": "2026-07-25T10:22:51-04:00", "currency": "EUR",
    "subtotal_price": "29.70", "total_tax": "0.00", "total_price": "34.60",
    "note": "leave at the door", "email": "jane@example.com",
    "customer": {"first_name": "Jane", "last_name": "Doe"},
    "shipping_address": {"name": "Jane Doe", "address1": "Musterweg 1", "address2": "",
                         "zip": "12345", "city": "Berlin", "country": "Germany",
                         "phone": "+49 30 123456"},
    "shipping_lines": [{"title": "Standard", "price": "4.90"}],
    "line_items": [{"quantity": 2, "sku": "A-1", "title": "Widget", "variant_title": "blue",
                    "price": "9.90"},
                   {"quantity": 1, "sku": "B-2", "title": "Gadget", "variant_title": "Default Title",
                    "price": "9.90"}],
}

WOO = {
    "id": 727, "number": "727", "date_created": "2026-07-25T10:22:51", "currency": "EUR",
    "total": "34.60", "shipping_total": "4.90", "total_tax": "0.00", "discount_total": "0.00",
    "customer_note": "leave at the door",
    "billing": {"email": "jane@example.com", "phone": "+49 30 123456"},
    "shipping": {"first_name": "Jane", "last_name": "Doe", "address_1": "Musterweg 1",
                 "address_2": "", "postcode": "12345", "city": "Berlin", "country": "DE"},
    "line_items": [{"name": "Widget", "quantity": 2, "sku": "A-1", "total": "19.80"},
                   {"name": "Gadget", "quantity": 1, "sku": "B-2", "total": "9.90"}],
}


def test_shopify_order_maps_to_the_slip_shape():
    o = normalize_order(SHOPIFY, "shopify")
    assert o["number"] == "#1001"
    assert o["date"] == "2026-07-25"
    assert o["customer"] == "Jane Doe"
    assert o["address"] == ["Musterweg 1", "12345 Berlin", "Germany"]
    assert o["email"] == "jane@example.com" and o["phone"] == "+49 30 123456"
    assert o["note"] == "leave at the door"
    # variant is appended, except Shopify's "Default Title" placeholder
    assert [line["name"] for line in o["lines"]] == ["Widget (blue)", "Gadget"]
    # Shopify only sends a unit price, so the line amount is qty x price.
    # Every value is a string — the slip prints text, it never does arithmetic on these.
    assert o["lines"][0] == {"qty": "2", "sku": "A-1", "name": "Widget (blue)",
                             "total": "19.80 EUR"}
    assert o["totals"] == [["Subtotal", "29.70 EUR"], ["Shipping", "4.90 EUR"],
                           ["Total", "34.60 EUR"]]
    assert o["source_id"] == "450789469"


def test_woocommerce_order_maps_to_the_slip_shape():
    o = normalize_order(WOO, "woocommerce")
    assert o["number"] == "727" and o["date"] == "2026-07-25"
    assert o["customer"] == "Jane Doe"
    assert o["address"] == ["Musterweg 1", "12345 Berlin", "DE"]
    assert o["lines"][0] == {"qty": "2", "sku": "A-1", "name": "Widget", "total": "19.80 EUR"}
    assert o["totals"] == [["Shipping", "4.90 EUR"], ["Total", "34.60 EUR"]]
    assert o["source_id"] == "727"


def test_zero_and_missing_amounts_are_dropped_not_printed_as_zero():
    o = normalize_order(dict(WOO, shipping_total="0.00", total_tax="2.10"), "woocommerce")
    labels = [label for label, _ in o["totals"]]
    assert labels == ["Tax", "Total"]          # no 0.00 shipping row


def test_already_normalized_orders_pass_through_validated():
    o = normalize_order({"number": 55, "lines": [{"qty": 1, "name": "Thing"}],
                         "totals": [["Total", "1.00"]], "extra": "ignored"}, None)
    assert o["number"] == "55" and o["lines"] == [{"qty": "1", "name": "Thing"}]
    assert "extra" not in o


def test_rejects_junk():
    with pytest.raises(OrderError):
        normalize_order("not a dict", None)
    with pytest.raises(OrderError):
        normalize_order({}, None)                       # no order number
    with pytest.raises(OrderError):
        normalize_order({"number": "1", "lines": {}}, None)
    with pytest.raises(OrderError, match="unknown order format"):
        normalize_order(SHOPIFY, "magento")


def test_line_count_is_capped():
    huge = {"number": "1", "lines": [{"qty": 1, "name": f"x{i}"} for i in range(1001)]}
    with pytest.raises(OrderError, match="too many line items"):
        normalize_order(huge, None)


def test_shopify_without_shipping_address_falls_back_to_the_customer():
    payload = dict(SHOPIFY)
    del payload["shipping_address"]
    o = normalize_order(payload, "shopify")
    assert o["customer"] == "Jane Doe" and o["address"] == []
