# printpapi — self-hosted PrintNode alternative. MIT License (see LICENSE).
"""Order dict -> packing-slip PDF, stdlib only (the server has no dependencies).

# ponytail: a deliberately small PDF writer — Helvetica/WinAnsi text and hairlines, no images,
# no embedded fonts, no compression, and column widths estimated at 0.5 em per character instead
# of real font metrics. That is enough for a packing slip; if someone needs a logo, a non-Latin
# script or real typography, render the document elsewhere and submit it as pdf_uri instead.
"""

A4 = (595, 842)          # points
_M = 40                  # page margin
_LEAD = 14               # line height
# Rough Helvetica advance per character. Deliberately above the ~0.5 em average (digits and
# capitals are 0.556–0.72): over-estimating truncates a little early and right-aligns a little
# left, which keeps columns from colliding. Under-estimating would overlap them.
_EM = 0.58

# The normalized order this renders (every field optional except `number`):
#   {"number": "1001", "date": "2026-07-25", "shop": "Acme", "customer": "Jane Doe",
#    "email": ..., "phone": ..., "address": ["Street 1", "12345 City"],
#    "lines": [{"qty": 2, "sku": "A-1", "name": "Widget", "total": "19.80"}],
#    "totals": [["Subtotal", "29.70"], ["Total", "34.60"]], "note": "leave at the door"}
# Money is passed through as text — the shop already did the arithmetic, we never redo it.


def _esc(s):
    """Text -> PDF string body: WinAnsi-encodable, with (, ) and \\ escaped."""
    s = str(s).encode("cp1252", "replace").decode("cp1252")
    return s.replace("\\", "\\\\").replace("(", "\\(").replace(")", "\\)")


def _width(s, size):
    return len(str(s)) * _EM * size


def _fit(s, avail, size):
    """Truncate to what fits in `avail` points (ellipsis if cut)."""
    s = str(s)
    limit = max(1, int(avail / (_EM * size)))
    return s if len(s) <= limit else s[:limit - 1] + "…"


def _text(x, y, s, size=10, bold=False):
    return f"BT /{'F2' if bold else 'F1'} {size} Tf {x:.0f} {y:.0f} Td ({_esc(s)}) Tj ET\n"


def _right(x, y, s, size=10, bold=False):
    return _text(x - _width(s, size), y, s, size, bold)


def _rule(x0, y, x1):
    return f"0.5 w {x0:.0f} {y:.0f} m {x1:.0f} {y:.0f} l S\n"


def _pdf(streams, page_size):
    """Assemble content streams into a PDF: catalog, page tree, two fonts, then page+stream pairs."""
    w, h = page_size
    page_ids = [5 + 2 * i for i in range(len(streams))]
    objs = [
        "<< /Type /Catalog /Pages 2 0 R >>",
        "<< /Type /Pages /Kids [%s] /Count %d >>"
        % (" ".join(f"{i} 0 R" for i in page_ids), len(streams)),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>",
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica-Bold /Encoding /WinAnsiEncoding >>",
    ]
    for i, stream in enumerate(streams):
        objs.append(f"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {w} {h}] /Resources "
                    f"<< /Font << /F1 3 0 R /F2 4 0 R >> >> /Contents {page_ids[i] + 1} 0 R >>")
        objs.append(("STREAM", stream))

    out = bytearray(b"%PDF-1.4\n")
    offsets = []
    for n, obj in enumerate(objs, start=1):
        offsets.append(len(out))
        if isinstance(obj, tuple):
            data = obj[1].encode("cp1252", "replace")
            out += (f"{n} 0 obj\n<< /Length {len(data)} >>\nstream\n").encode()
            out += data + b"\nendstream\nendobj\n"
        else:
            out += f"{n} 0 obj\n{obj}\nendobj\n".encode()
    xref = len(out)
    out += b"xref\n0 %d\n" % (len(objs) + 1)
    out += b"0000000000 65535 f \n"
    for off in offsets:
        out += b"%010d 00000 n \n" % off
    out += b"trailer\n<< /Size %d /Root 1 0 R >>\nstartxref\n%d\n%%%%EOF\n" % (len(objs) + 1, xref)
    return bytes(out)


def render_packing_slip(order, page=A4, title="Packing slip"):
    """Render a normalized order (see the shape above) as a one- or many-page PDF."""
    order = order or {}
    w, h = page
    num = str(order.get("number", "")).strip()
    lines = order.get("lines") or []
    totals = order.get("totals") or []
    col_sku, col_name = _M + 34, _M + 110
    name_width = w - _M - col_name - 82          # last 70pt are the amount, plus a 12pt gutter
    floor = _M + 40                              # below this a page is full

    pages, body = [], []
    y = 0

    def open_page(first):
        nonlocal body, y
        body = []
        if first:
            body.append(_text(_M, h - _M - 14, title, 18, bold=True))
            if order.get("shop"):
                body.append(_right(w - _M, h - _M - 14, order["shop"], 10))
            body.append(_text(_M, h - _M - 40, f"Order {num}" if num else "Order", 12, bold=True))
            if order.get("date"):
                body.append(_right(w - _M, h - _M - 40, order["date"], 10))
            y = h - _M - 66
            for line in (order.get("customer"), *(order.get("address") or []),
                         order.get("email"), order.get("phone")):
                if line:
                    body.append(_text(_M, y, _fit(line, w - 2 * _M, 10), 10))
                    y -= _LEAD
            y -= 12
        else:
            body.append(_text(_M, h - _M - 14, f"Order {num} (continued)", 12, bold=True))
            y = h - _M - 44
        body.append(_text(_M, y, "Qty", 10, bold=True))
        body.append(_text(col_sku, y, "SKU", 10, bold=True))
        body.append(_text(col_name, y, "Item", 10, bold=True))
        body.append(_right(w - _M, y, "Amount", 10, bold=True))
        y -= 5
        body.append(_rule(_M, y, w - _M))
        y -= _LEAD

    def close_page():
        pages.append("".join(body))

    open_page(True)
    for item in lines:
        if y < floor:
            close_page()
            open_page(False)
        body.append(_text(_M, y, item.get("qty", ""), 10))
        body.append(_text(col_sku, y, _fit(item.get("sku", ""), col_name - col_sku - 6, 10), 10))
        body.append(_text(col_name, y, _fit(item.get("name", ""), name_width, 10), 10))
        if item.get("total") is not None:
            body.append(_right(w - _M, y, item["total"], 10))
        y -= _LEAD

    needed = len(totals) * _LEAD + (2 * _LEAD if order.get("note") else 0) + 20
    if totals or order.get("note"):
        if y - needed < _M:                    # totals must not be orphaned mid-page
            close_page()
            open_page(False)
        y -= 5
        body.append(_rule(w - _M - 200, y, w - _M))
        y -= _LEAD + 2
    for i, (label, amount) in enumerate(totals):
        last = i == len(totals) - 1
        body.append(_text(w - _M - 200, y, label, 10, bold=last))
        body.append(_right(w - _M, y, amount, 10, bold=last))
        y -= _LEAD
    if order.get("note"):
        y -= _LEAD
        # ponytail: notes are a single truncated line — wrap them if shops start writing essays.
        body.append(_text(_M, y, _fit(f"Note: {order['note']}", w - 2 * _M, 10), 10))
    close_page()

    stamped = [p + _text(_M, _M - 16, f"Page {i}/{len(pages)}", 8)
               for i, p in enumerate(pages, start=1)]
    return _pdf(stamped, page)
