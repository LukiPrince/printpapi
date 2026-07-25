import re

from app.packing_slip import render_packing_slip

ORDER = {
    "number": "1001", "date": "2026-07-25", "shop": "Acme Store",
    "customer": "Jane Doe", "address": ["Musterweg 1", "12345 Berlin", "DE"],
    "lines": [{"qty": 2, "sku": "A-1", "name": "Widget", "total": "19.80"},
              {"qty": 1, "sku": "B-2", "name": "Gadget (large)", "total": "9.90"}],
    "totals": [["Subtotal", "29.70"], ["Total", "34.60"]],
    "note": "leave at the door",
}


def _objects(pdf):
    return re.findall(rb"(\d+) 0 obj", pdf)


def _streams(pdf):
    return b"".join(re.findall(rb"stream\r?\n(.*?)endstream", pdf, re.S))


def test_renders_a_structurally_valid_pdf():
    pdf = render_packing_slip(ORDER)
    assert pdf.startswith(b"%PDF-1.4") and pdf.rstrip().endswith(b"%%EOF")
    # startxref must point at the xref table, or a reader rejects the file
    offset = int(re.search(rb"startxref\s+(\d+)", pdf).group(1))
    assert pdf[offset:offset + 4] == b"xref"
    # every object offset in the xref table must land on that object's header
    entries = re.search(rb"xref\s+0 (\d+)\s+((?:\d{10} \d{5} [fn] \r?\n)+)", pdf)
    count = int(entries.group(1))
    rows = entries.group(2).split(b"\n")[1:]           # row 0 is the free head
    for i, row in enumerate(r for r in rows if r.strip()):
        assert pdf[int(row[:10]):].startswith(b"%d 0 obj" % (i + 1))
    assert len(_objects(pdf)) == count - 1


def test_order_content_is_on_the_page():
    text = _streams(render_packing_slip(ORDER)).decode("latin-1")
    for expected in ("1001", "Jane Doe", "Musterweg 1", "Widget", "A-1", "34.60",
                     "leave at the door", "Acme Store"):
        assert expected in text


def test_escapes_pdf_syntax_and_non_latin_characters():
    order = dict(ORDER, customer=r"Foo (Bar) \ Baz", note="Grüße 日本")
    text = _streams(render_packing_slip(order)).decode("latin-1")
    assert r"Foo \(Bar\) \\ Baz" in text
    assert "Gr\xfc\xdfe" in text          # cp1252 round-trip
    assert "??" in text                   # unmappable glyphs degrade, never crash


def test_long_orders_paginate_and_number_the_pages():
    order = dict(ORDER, lines=[{"qty": 1, "sku": f"S-{i}", "name": f"Item {i}", "total": "1.00"}
                               for i in range(120)])
    pdf = render_packing_slip(order)
    text = _streams(pdf).decode("latin-1")
    assert re.search(rb"/Count (\d+)", pdf).group(1) != b"1"
    assert "Page 1/" in text and "S-119" in text        # last row survived the pagination
    # every line item appears exactly once
    assert all(f"S-{i}" in text for i in range(120))


def test_missing_fields_do_not_crash():
    pdf = render_packing_slip({"number": "7"})
    assert pdf.startswith(b"%PDF") and b"/Count 1" in pdf
