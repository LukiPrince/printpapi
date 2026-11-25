# Receipts from JSON

A job of type `receipt` is a structured document: texts, columns, a divider, a logo, a QR code,
a barcode, a cut, the cash drawer. The **server lays it out once** and writes it as ESC/POS bytes
for a receipt printer, as a PDF for a printer that takes PDF, or as a preview image. The agent is
not involved: it gets an ordinary `raw` or `pdf` job, so every agent (the Rust one and the
legacy Python one) works with it.

> **Not yet verified on a physical printer.** Everything here is built to the published ESC/POS
> commands and checked against golden files and a preview, as the [CloudPRNT](cloudprnt.md)
> support is. Speed, cutter and drawer behaviour and the printer's own font need a real 58 mm and
> 80 mm printer. Until you have tried one, use the **Test receipt** button on the Devices page
> (see [Dashboard](#dashboard)): it saves the printer's settings and prints a sample that shows a
> wrong setting at once. [What is not verified](#what-is-not-verified) lists the rest.

```bash
curl -s -X POST localhost:3460/jobs \
  -H 'Authorization: Bearer <client-key>' -H 'Content-Type: application/json' \
  -d '{"printer_id": 3, "type": "receipt", "title": "Bon 4711", "idempotency_key": "bon-4711",
       "receipt": {"cut": true, "content": [
         {"type": "text", "text": "Café Müller", "align": "center", "bold": true, "size": 2},
         {"type": "divider"},
         {"type": "columns", "cells": [{"text": "2 x Weißbrot"},
                                       {"text": "5,80 €", "align": "right", "width": 10}]},
         {"type": "qr", "data": "https://example.com/r/4711"},
         {"type": "barcode", "format": "ean13", "data": "4006381333931"},
         {"type": "drawer"}]}}'
# -> {"job_id": 7}
```

## The job

`POST /jobs` with `"type": "receipt"` and a `receipt` object. Every other job field works as it does
for any job: `printer_id`, `title`, `copies`, `callback_url`, `idempotency_key`, `expire_after`, and
`options` when the output is a PDF (see [Submitting a job](api.md#submitting-a-job)). A resubmit
with the same `idempotency_key` returns the original job without rendering again.

`copies` repeats the **whole byte stream**, the drawer kick included: `copies: 2` opens the drawer
twice. Send the drawer in a job of its own if that matters.

The `receipt` object has `content` (a list of 1 to 500 elements, required) and `cut` (true by
default). Unknown fields are refused, as everywhere in the API.

## Elements

| `type` | Fields (default) |
|---|---|
| `text` | `text` (required, 1 to 4000 characters, `\n` starts a new line); `align` `left` \| `center` \| `right` (`left`); `bold` (false); `size`: a whole number 1 to 8, or `[w, h]` (1) |
| `columns` | `cells`: 2 to 6 objects `{text, align, bold, width}`; `size` (1). `width` is in characters **of this row's size** |
| `divider` | `char` (`-`): exactly one printable character, repeated over the full width |
| `image` | `content`: base64 GIF, PNG or JPEG; `align` (`center`); `width_percent` 1 to 100 (absent: natural size in dots, shrunk to the paper, never enlarged); `dither` (false) |
| `qr` | `data` (required, 1 to 2000 bytes); `ec` `L` \| `M` \| `Q` \| `H` (`M`); `size`: dots per module, 1 to 16 (absent: the largest of 8 down to 2 that fits); `align` (`center`) |
| `barcode` | `format` `code128` or `ean13`; `data`; `height` 24 to 240 dots (80); `size`: dots per module 1 to 6 (absent: the largest of 3 down to 2 that fits); `text` (true: the number as a text row below, centred under the bars; a number longer than a line goes on to the next row); `align` (`center`) |
| `feed` | `lines` 1 to 20 (1) |
| `cut` | nothing |
| `drawer` | `pin` 2 or 5 (2) |

The paper is a grid of cells 12 dots wide and 24 high; a line of text takes 30 dots (24 times the
height factor from size 2 on). At 80 mm a line has 48 characters, at 58 mm 32 (see
[Printer profile](#printer-profile)). Alignment is padding: spaces in a text row, a dot offset for
an image. `ESC a` is never used.

- **Columns.** One blank character sits between cells. A cell with a `width` takes that many
  characters; the cells without one share the rest equally, the remainder going to the first of
  them. Each cell wraps inside its own width and the row is as high as the tallest cell. A row that
  does not fit is `400` `receipt.content[4]: columns do not fit 32 characters`.
- **QR codes.** Always drawn as a bitmap, with a quiet zone of 4 modules. The encoder picks numeric,
  alphanumeric or byte mode by itself, so digits fit more than text. The error correction level you
  ask for is the least that is used: when a higher level fits the same size (version) of code, the
  code is made at that one, as `QrCode::encode_text` does, so `ec` `M` on a short text prints at
  `H`. A text that does not fit at the level you asked for is `400` `qr data is too long for ec
  H`. At 2000 bytes of ordinary text `L` and `M` fit
  (the version 40 capacity is 2331 bytes at `M`, 1663 at `Q`, 1273 at `H`), `Q` and `H` do not.
- **Barcodes.** `code128` takes 1 to 48 characters of printable ASCII; all digits with an even
  count use the dense character set C, anything else set B (no mixed-set optimisation). `ean13` takes
  12 digits (the check digit is added) or 13 (verified, `400` `ean13 check digit is wrong` if it is
  not). UPC-A is an EAN-13 with a leading `0`; it has no format of its own. The paper limits the
  width, with a quiet zone of 10 modules on each side: 48 characters of set B are 563 modules and
  never fit, about 21 fit 576 dots at 2 dots per module. A barcode that does not fit is
  `400` `barcode does not fit 576 dots`.
- **Images.** The server decodes the image (EXIF orientation applied, transparency printed white)
  and turns it into black and white. `dither` uses Floyd-Steinberg dithering, which suits photos; a
  logo or a drawing is better without it.
- **`feed`** advances the paper by lines of 30 dots. `cut` cuts there: the profile decides between a
  partial cut, a full cut and, for a printer without a cutter, a feed of 4 lines.

### The automatic cut, and documents that print nothing

With `"cut": true` a cut is added at the end only if something was printed (a row, an image, a QR
code, a barcode or a feed) after the last cut you wrote yourself. So:

- `[text]` gets a cut; `[text, cut]` does not get a second one; `{"cut": false}` never gets one.
- `[{"type": "drawer"}]` opens the till and feeds and cuts **nothing**; `[text, cut, drawer]` opens it
  after the cut.
- A receipt of a `feed` alone feeds and cuts.

On a PDF printer, a page exists only for a section between cuts that has at least one row or
image. A leading cut, a double cut and a trailing cut or feed make no page. If no page is left the
answer is `400` `receipt prints nothing on a PDF printer` (the preview says the same).

### Text

Texts are cleaned before they are laid out: `\r\n` and `\r` become `\n`, a tab, a no-break space and a
narrow no-break space (U+202F) become a space, other control characters are dropped, and so are
the zero-width and invisible characters (U+200B to U+200F, the direction marks U+202A to U+202E and
U+2066 to U+2069, U+2060 to U+2064, U+FE00 to U+FE0F, U+FEFF, U+00AD). A base letter followed by a
combining accent (a `u` and U+0308) is composed to `ü`; any other combining mark is dropped; `ẞ`
(U+1E9E) becomes `SS`. No control character, `ESC` and `GS` included, ever reaches the printer as a
command. A paragraph that fits a line is kept as it is; a longer one breaks at the last space that
fits, or after the last character that fits when there is no space. Text reads left to right only.

A character the font has no glyph for (an emoji, a CJK character) prints as an empty box. It never
refuses the job: an emoji in a customer's note must not stop a receipt. The preview shows the same
box.

## Limits

| What | Limit |
|---|---|
| Elements | 500 |
| Text in all, in characters | 20 000 |
| Images | 8 |
| QR codes and barcodes together | 20 |
| One image file, decoded | 1 MiB |
| All image files of one receipt | 2 MiB |
| One image, in pixels (read from the header) | 4 megapixels, 64 MiB of decoder memory |
| Output, in dots of paper | 20 000 rows (2.5 m); with `cut: none` in the profile every cut counts its 4 fed lines (120 dots) |

The running total of rows is checked after every element and before the next code is encoded or
image decoded, so a long receipt is refused early.

## Errors

All `400` with the path of the problem, for example:

- `receipt.content[3].size must be 1-8 or [w, h]`
- `receipt.content[2]: unknown type "foo"`
- `unknown field in receipt.content[0]: foo`
- `receipt.content[4]: columns do not fit 32 characters`
- `receipt.content[5]: qr does not fit 384 dots`
- `receipt.content[5]: qr data is too long for ec H`
- `receipt.content[5]: barcode does not fit 576 dots`
- `receipt.content[5]: ean13 check digit is wrong`
- `receipt.content[1].char must be one printable character`
- `receipt.content[6]: image is too large (5000x3000, at most 4000000 pixels)`
- `receipt is longer than 20000 dots`
- `receipt prints nothing on a PDF printer`
- `missing 'receipt'`
- `CloudPRNT printers cannot print receipts — send raw Star commands`

`402` for a spent quota and `403` for a locked org, as for every job. `503` `receipt rendering is
busy, try again later` only for a receipt **with images**, see
[Concurrent image receipts](#concurrent-image-receipts); `503` `preview is busy, try again shortly`
for the previews.

## Printer profile

How receipts to one printer are laid out is stored **per printer on the server**: not in
`agent.ini` (the legacy agent is frozen) and not with the printer's capabilities (the agent sends
those again at every start). A printer without a stored profile has the defaults.

```json
{"paper_mm": 80, "width_dots": 576, "codepage": "raster", "codepage_number": 16, "cut": "partial", "output": "auto"}
```

| Field | Values (default) |
|---|---|
| `paper_mm` | 58 or 80 (80) |
| `width_dots` | a multiple of 8 from 192 up to `paper_mm * 8`: 464 on 58 mm paper, 640 on 80 mm paper (for a head wider than the usual print width). The default is the usual width of the paper: 384 (58 mm) and 576 (80 mm) |
| `codepage` | `raster` (default): every text row is drawn. `cp1252`: native text where possible, see [Drawn text and native text](#drawn-text-and-native-text) |
| `codepage_number` | 0 to 255 (16): the `n` of `ESC t n` for `cp1252`, ignored with `raster` |
| `cut` | `partial` (default), `full` or `none` (feeds 4 lines instead of cutting) |
| `output` | `auto` (default): a PDF on a printer that takes PDF, ESC/POS on every other. `escpos`: always ESC/POS |

Characters per line are `width_dots / 12`: 48 at 576 dots, 42 at 504, 32 at 384.

| Route | Right | Answer |
|---|---|---|
| `GET /printers/{id}/receipt-profile` | `Look` | `{"printer_id": 3, "stored": false, "profile": {...}}` (`stored` is false while the defaults apply) |
| `PUT /printers/{id}/receipt-profile` | `Edit` | the same with `stored: true`. The body replaces the **whole** profile: omitted fields take the defaults. `400` for an invalid one |
| `DELETE /printers/{id}/receipt-profile` | `Edit` | `{"ok": true}`, the defaults apply again; also for a printer that had none |

`404` for an unknown or foreign printer, `400` for a CloudPRNT device. A profile survives every
re-registration of the agent: printers are found again by their agent and name, so it belongs to
that printer. A printer you rename in `agent.ini` is a new printer with the default settings. A
profile goes away with its printer, which is only deleted with its org.

There is no `pdf` value for `output`. A raw printer can never be sent a PDF, which would print
blank paper (see [the one rule](agent.md#labels-vs-documents-the-one-rule)).

## What reaches the printer

- A raw printer, or `"output": "escpos"`: job mode `raw`, the ESC/POS bytes below.
- A printer that takes PDF (`|pdf`) with `"output": "auto"`: job mode `pdf`, one page for every
  section between cuts that has something on it. `options` (duplex, tray, ...) are accepted here and
  refused for ESC/POS (`options are only supported on pdf jobs`).
- A **CloudPRNT** device is refused: it speaks Star commands, see [cloudprnt.md](cloudprnt.md).
- The [PrintNode layer](printnode-compat.md) does not take receipts; a receipt job shows up in its
  job list with `contentType` `receipt`.

### The ESC/POS stream

The stream starts with `ESC @` (reset) and, with `cp1252`, `ESC R 0` and `ESC t n`. Then, per
element: a native text row (`GS ! n` for the size and `ESC E n` for bold, each only when it
changes, then the bytes and `LF`); drawn rows, images, QR codes and barcodes as `GS v 0` raster
bitmaps, consecutive ones drawn into one bitmap and sent in bands of at most 128 rows, the right
side of a band trimmed to its last black byte; `ESC d n` for a feed; `GS V 66 0` (partial) or
`GS V 65 0` (full) for a cut; `ESC p m 25 250` for the drawer. The stream **ends with `ESC @`** too,
so no code page, size or bold stays behind for the next raw job of another client. These are the
only commands written, and the printer needs `GS v 0`: impact models such as the TM-U220 have none
(7 of the 50 printers in the open [escpos-printer-db](https://github.com/receipt-print-hq/escpos-printer-db)).

### PDF pages

A PDF page is as wide as the paper (`paper_mm`) and **always taller than wide**: the receipt is
centred at 8 dots per millimetre with 4 mm above and below, and the page is at least
`paper_mm + 1` mm tall, white at the bottom. The server never picks an orientation, and SumatraPDF
turns a page that is wider than tall when the driver reports portrait (see
[Submitting a job](api.md#submitting-a-job)). A drawer does nothing in a PDF.

A real receipt printer that is also marked `|pdf` (a driver-based one) is better off with
`"output": "escpos"`: it prints the ESC/POS bytes directly, faster and at its real width. How
SumatraPDF and CUPS scale a 58 or 80 mm wide long page on such a driver is not verified.

### A virtual receipt printer

A `file://` printer is always `can_pdf`, so with the defaults it writes a viewable
`job-<id>.pdf` per receipt. That makes a receipt printer you can test with, and keep an archive of
(see [File output](agent.md#file-output-virtual-print-server)). Set `"output": "escpos"` on it to get
the `.prn` file with the ESC/POS bytes.

## Drawn text and native text

By default (`codepage: raster`) the server **draws every text row** with an embedded 12 x 24 font
and sends it as a bitmap. Umlauts, the euro sign and column alignment are then right on every
printer that supports `GS v 0`, whatever its code page table, double-byte mode or font width, and
the preview shows the same dots. Wrong characters or prices that run onto the next line? Use
`raster`.

`codepage: cp1252` is the faster opt-in. A row whose characters **all** have a Windows-1252 byte goes
out as native text in the printer's own font; a row with any other character is drawn as a whole
row in the same cells, so the layout does not move (so is a blank line of size 2 or more: an empty
line feed moves the paper one line only). It needs a printer whose font A has
`width_dots / 12` characters per line and that has Windows-1252. Windows-1252 is code page number 16
in 33 of the 50 printers of escpos-printer-db, so check the printer's manual and set
`codepage_number` if it is another number. A drawn line is about 1.7 KB at 80 mm, which is slow on
a 9600 baud serial printer: choose `cp1252` there.

The preview of a `cp1252` printer shows the drawn font; the printer's own looks slightly
different.

## Preview

`POST /receipts/preview` (right `Submit`) draws a receipt as a PNG and prints nothing. The body is
`{"receipt": {...}, "printer_id": 3}` (the stored profile of that printer), or `{"receipt": {...},
"profile": {...}}`, or just `{"receipt": {...}}` (the defaults). Both a printer and a profile is `400`; an
unknown or foreign printer is `400` `unknown printer: 3`. It spends no quota and creates no job.

```bash
curl -s -X POST localhost:3460/receipts/preview \
  -H 'Authorization: Bearer <client-key>' -H 'Content-Type: application/json' \
  -d '{"receipt": {"content": [{"type": "text", "text": "Hallo Welt", "size": 2}]}, "profile": {"paper_mm": 58}}' \
  -o preview.png
```

The answer is `image/png` with `Cache-Control: no-store` and `X-Content-Type-Options: nosniff`. A
preview has a small budget of its own, so a flood of them can never hold up a print: **one in flight
per org and two on the whole server**, else `503` `preview is busy, try again shortly`. The
dashboard keeps one request in flight and retries a `503` by itself. While the operator API is on,
an org that is locked gets `403`, as for a job.

### The preview of a job in the history

`GET /jobs/{id}/preview` draws a receipt job as it was printed, from a copy of its document and the
profile it was printed with. Only a login that can manage may read it (a session, or root); a
machine key, even a full one, gets `401`. A session sees its own org's jobs, root every org's, as
everywhere on a server without the operator API. `404` for an unknown or foreign job, a job that is no
receipt, and one whose content is gone.

- **Retention.** The copy goes **with the payload**. The database removes it when the payload is
  emptied by the [retention pass](server.md#retention) and when the job row is deleted, also by an
  older server that does not know the table, because the database does the cleanup. Deleting an org
  removes its copies and profiles the same way.
- **With the operator API there is none.** While `PRINTAPI_OPERATOR_TOKEN` is set (a hosted
  server), no copy is kept and this route always answers `404`. A session-readable copy of a
  customer's receipt would be a way round the audited [support access](operator.md#support-access):
  root can make a user with a known password in any org. There the operator reads what a job
  contains only under a grant the customer can see.

## Concurrent image receipts

A receipt **without images** takes no render slot: it costs milliseconds and little memory. A
receipt with images has to decode them, which can take tens of megabytes, so those use a pool of
4 slots of their own (labels have theirs, so neither blocks the other). A receipt waits up to about
two seconds for a slot and then answers `503` `receipt rendering is busy, try again later`. An image
receipt also counts against the org's limit of downloads in flight (see `PRINTAPI_FETCHES_PER_ORG` in
[server.md](server.md)) and, where that limit applies, one org renders at most 2 of the 4 at once
(a third receipt of the org waits for one of its own two slots inside the same two seconds, so
three receipts sent together, kitchen, bar and customer copy, all print, and one org still cannot
keep the pool busy for the others).
It holds its `idempotency_key` while it renders: a second request with the
same new key at that moment answers `409` (`a request with this idempotency_key is still in
progress, retry shortly`) and returns the job when you retry. A previously submitted key returns
the original job at once, without rendering.

## Dashboard

- **Print**, tab *Receipt*: the receipt as JSON, with a live preview below it (syntax errors are
  shown in the browser, everything else is the server's text). One line says what the chosen printer
  is set to, for example `80 mm, 48 characters, ESC/POS`.
- **Devices**, the receipt settings button on a printer: paper, print width, text mode, cutter and,
  for a printer that takes PDF, a switch to send ESC/POS instead. **Test receipt** saves the settings
  and prints a sample with umlauts, the euro sign, a price column, a QR code and a barcode. A CloudPRNT
  printer shows the server's reason and cannot be changed.
- **Print History**: *Show receipt* in the expanded row of a receipt job, where the operator API is off.

## What is not verified

No physical receipt printer has been tried. Open: raster speed and banding of `GS v 0` on clones and
on slow serial or Bluetooth links, cutter and drawer behaviour and feed distances, `ESC @` directly
after a cut on clones, the printer's own font and code page in `cp1252` mode, scanning the printed QR
codes and barcodes (the module patterns are tested, the optics are not), and how SumatraPDF and CUPS
scale a narrow long page. `GS v 0` is marked obsolete by Epson, and a few models lack it.

Left out for now: images by URL, a per-job profile, stored receipt templates, Star Line and StarPRNT
output, native `GS ( k` QR and `GS k` barcode commands, other code pages (drawn text covers those
printers), and PDF417, Data Matrix, Code 39 and CJK or emoji glyphs.

## Credits and licence

The drawn text uses **Terminus Font 4.49.1** (`ter-u24n.bdf` and `ter-u24b.bdf`), copyright (C)
2020 Dimitar Toshkov Zhekov, under the **SIL Open Font License 1.1**. The two files are embedded
unmodified, and the licence text is served by every server at
`/licenses/terminus-font-OFL.txt` (it is also in the repository as
`web/public/licenses/terminus-font-OFL.txt`). Source: the Terminus Font page on SourceForge,
`terminus-font-4.49.1.tar.gz`. The QR encoder is `qrcodegen` (MIT) and the barcode tables come from
`barcoders` (MIT or Apache-2.0).

ESC/POS is a trademark of Seiko Epson Corporation; printpapi is not affiliated with Epson. Star and
CloudPRNT belong to Star Micronics, see [cloudprnt.md](cloudprnt.md).
