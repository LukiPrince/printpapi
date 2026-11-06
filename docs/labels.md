# Label templates

A label template says **where the content sits on one kind of label**: the label size, how an
image is rotated and cropped, and the box it is placed in. You set it up once, then every job that
names the template is rendered by the server exactly that way. The client sends a picture or a
line of text and nothing else, so a shop, a warehouse script or an n8n flow does not need its own
render service.

Create and edit templates on the dashboard's **Labels** page: pick a size from the catalogue (or
type your own), drag and scale the content on a true-to-scale label, and save. The same thing is
available over HTTP, described below.

## What a template is

```json
{
  "name": "shipping-4x6",
  "content": "image",
  "label_mm": { "w": 101.6, "h": 152.4 },
  "dpi": 203,
  "rotate": 90,
  "crop_percent": { "t": 0, "r": 0, "b": 14.3, "l": 0 },
  "place_mm": { "x": 0.8, "y": 1.3, "w": 100.1, "h": 149.8 },
  "text": { "wrap_chars": 12, "lines_per_label": 2 },
  "zpl_darkness": 30,
  "size_ref": "Standard:4x6"
}
```

| Field | Meaning |
|---|---|
| `name` | 1–64 characters: letters, digits, space, `.`, `_`, `-`. Unique within the org. |
| `content` | `image` (for `label_uri` / `label_base64` jobs) or `text` (for `label_text` jobs). |
| `label_mm` | The label's width and height in millimetres, greater than 0. At `dpi` each edge must come to 1–8000 printer dots (`mm / 25.4 * dpi`, rounded). |
| `dpi` | The printer's resolution, 100–600. Usually 203 or 300. |
| `rotate` | `0`, `90`, `180` or `270` degrees **clockwise**, applied to the image first. |
| `crop_percent` | How much to cut off the (rotated) image on each side, `t` `r` `b` `l`, each 0–95. `t + b` and `l + r` must stay below 100. |
| `place_mm` | The box on the label that the cropped image is scaled into, in millimetres from the top left. `x` and `y` may be negative and the box may stick out over the edge: what lies outside the label is cut off. |
| `text` | Optional. `wrap_chars` (word wrap, 0 = no wrap) and `lines_per_label` (0 = all lines on one label), each 0–200. Only used by `content: text`. Defaults to `{"wrap_chars": 0, "lines_per_label": 0}`. |
| `zpl_darkness` | Optional, -30 to 30. Written as `^MD` into ZPL output. Ignored for PDF. |
| `size_ref` | Optional, up to 64 characters. The dashboard stores which catalogue size you picked here; the server only passes it through. |

Unknown fields are refused with `400`. An image template ignores `text`; a text template ignores
`rotate` and `crop_percent`.

### Endpoints

All of them need a client key (or a session) and only ever see the caller's own org. A template of
another org behaves like one that does not exist.

| Method & path | Result |
|---|---|
| `GET /labels` | `{"templates": [{"id": 1, ...template, "created_at": ..., "updated_at": ...}]}`, sorted by name |
| `POST /labels` | `200` with the stored template and its `id`; `400` if it is invalid; `409` `{"error": "label template name already in use"}` |
| `GET /labels/{id}` | The template, or `404` |
| `PUT /labels/{id}` | Replaces the whole template. `400`, `404` and `409` as above. |
| `DELETE /labels/{id}` | `{"ok": true}`, or `404` |

```bash
curl -s -X POST localhost:3460/labels \
     -H 'Authorization: Bearer <client-key>' -H 'Content-Type: application/json' \
     -d '{"name":"shipping-4x6","content":"image","label_mm":{"w":101.6,"h":152.4},"dpi":203,
          "rotate":90,"crop_percent":{"t":0,"r":0,"b":14.3,"l":0},
          "place_mm":{"x":0.8,"y":1.3,"w":100.1,"h":149.8}}'
# -> {"id": 1, "name": "shipping-4x6", ... "created_at": 1767225600.0, "updated_at": 1767225600.0}
```

Deleting a template does not touch jobs that were rendered with it; they are ordinary queued jobs.

## Printing with a template

Three job types on `POST /jobs`. Each takes a `template`: the template's **name** (a string) or its
**id** (an integer). Everything else a job can carry works as usual: `copies`, `title`,
`idempotency_key`, `expire_after`, `callback_url` (see [api.md](api.md#submitting-a-job)).

| `type` | Also required | Template `content` |
|---|---|---|
| `label_uri` | `url`: the server fetches the image (`http(s)` only) | `image` |
| `label_base64` | `content`: the image, base64 | `image` |
| `label_text` | `text`: up to 500 characters | `text` |

Images may be GIF, PNG or JPEG. For an animated GIF only the first frame is used. Transparent
areas print white.

```bash
# an image from a URL
curl -s -X POST localhost:3460/jobs \
     -H 'Authorization: Bearer <client-key>' -H 'Content-Type: application/json' \
     -d '{"printer_id":1,"type":"label_uri","template":"shipping-4x6",
          "url":"https://shop.example/orders/4712/label.gif","title":"Order #4712"}'
# -> {"job_id": 7}

# an image you already hold
curl -s -X POST localhost:3460/jobs \
     -H 'Authorization: Bearer <client-key>' -H 'Content-Type: application/json' \
     -d "{\"printer_id\":1,\"type\":\"label_base64\",\"template\":1,
          \"content\":\"$(base64 -w0 label.png)\"}"

# a line of text on a text template (here: wrap at 12 characters, two lines per label)
curl -s -X POST localhost:3460/jobs \
     -H 'Authorization: Bearer <client-key>' -H 'Content-Type: application/json' \
     -d '{"printer_id":2,"type":"label_text","template":"error-code",
          "text":"Module Replacement needed","copies":2}'
```

On macOS use `base64 -i label.png` instead of `base64 -w0 label.png`.

The job that lands in the queue is an ordinary one: the server stores the **rendered result**, the
agent downloads and prints it like any other payload, and nothing on the agent changes. The job's
`type` in the history stays `label_uri`, `label_base64` or `label_text`.

### Order of checks and errors

The server checks the job fields, then looks up the printer, then the template, then checks that
the template's `content` matches the job type, and only then fetches the image. A bad printer or
template therefore never causes a download.

| Situation | Answer |
|---|---|
| Missing or malformed `template` / `text` / `url` / `content` | `400` |
| Unknown printer | `400` `unknown printer: <id>` |
| Unknown template, or one that belongs to another org | `400` `unknown label template: <name or id>` |
| A text job on an image template, or the other way round | `400` `label template <name> does not take text jobs` (or `image jobs`) |
| The image is not a GIF, PNG or JPEG, is larger than 40 megapixels or 20 000 pixels on one edge, would need too much memory to scale into the template's box, or the text is empty or over 500 characters | `400` `label: <reason>` |
| The image URL could not be fetched | `502` |
| Several image renders are already running (the server renders four at a time) | `503` `label rendering is busy, try again later`; retry |
| `options` on a printer that gets ZPL | `400` `options are only supported on pdf jobs` |
| A [CloudPRNT](cloudprnt.md) printer | `400` `CloudPRNT printers cannot print label templates — send raw Star commands` |

## What reaches the printer

The **printer** decides the output, not the template:

- A printer marked `|pdf` in `agent.ini` (that is how a driver-based label printer such as a DYMO
  LabelWriter is set up, see [agent.md](agent.md#dymo-labelwriter--pdf-not-raw)) gets a **PDF**
  whose page is exactly the label size. The content is a 1-bit image, so it prints sharp on a thermal printer. `options`
  (paper, tray, and so on) work as for any PDF job.
- Every other printer is raw and gets **ZPL**: `^XA^PW...^LL...` with the label size and, if the
  template sets `zpl_darkness`, `^MD`. An image goes out as `^GF` graphic fields in horizontal
  bands, text as `^A0N` lines. A raw printer never gets a PDF, which would print blank labels
  (see [agent.md](agent.md#labels-vs-documents-the-one-rule)).

"Raw" therefore means **ZPL** here. A raw printer that does not speak ZPL, such as an ESC/POS
receipt printer, cannot print from a template: it would be handed ZPL commands it does not
understand. Send those printers their own commands with `raw_base64` or `raw_uri`.

### Images

The server decodes the image, rotates it, crops it, scales it into the `place_mm` box and lays
that on a white label of the template's size. Whatever lies outside the label is cut off. A JPEG's EXIF
orientation is applied before the template's `rotate`, as a phone or browser shows the picture. The
result is reduced to black and white (a pixel is black when it is darker than about 55 %).

If the box has exactly the size of the cropped image in dots, no scaling happens, so a 1-dot line
stays one dot wide. Scale line art and barcodes to whole multiples of their size if they must
stay readable; a barcode that is scaled by a fraction can blur.

### Text

The text is cleaned first: `^` and `~` become spaces so no ZPL command can slip into a label,
control characters are dropped and runs of whitespace become one space. It is then wrapped at
`wrap_chars`, grouped into `lines_per_label` lines per label and drawn into the `place_mm` box,
left aligned and centred vertically. One job can therefore produce several labels: ZPL labels are
chained one after the other, PDF labels are pages. `copies` repeats the whole job.

The font height is picked so the longest line and all lines of a label fit the box, between 10 and
400 dots. Text that would need less than 10 dots does not fit: the job is refused with a 400 rather
than printing a label with lines missing. ZPL uses the printer's scalable font 0 and switches to
UTF-8 (`^CI28`) only for text outside ASCII, so umlauts survive on firmware that has it. PDF uses
Helvetica Bold in Windows-1252: characters outside it print as `?`.

A box that hangs off the label is cut off in a PDF. In ZPL a line that starts above the top edge or
below the bottom edge is left out, and a box that hangs over the left edge starts its text at the
left edge instead (ZPL has no negative origin).

### Printer settings that matter

- **Resolution.** ZPL is written in dots, and the server cannot ask a printer for its resolution.
  A template set to 203 dpi comes out about a third smaller on a 300 dpi printer, so `dpi` has to
  match the printer. PDF output keeps its physical size either way.
- **Wide labels on Windows.** When the driver reports portrait, SumatraPDF turns a page wider than
  it is tall by 90 degrees. On a tall sheet that is right, but if the driver's paper is as wide as
  the label, the label comes out sideways and small. Send `"options": {"orientation": "landscape"}`
  with such jobs, see [api.md](api.md#submitting-a-job). The server never sets this by itself,
  because on a tall sheet the same option halves the label. ZPL output is not affected, and CUPS
  turns a page only when it does not fit the paper.
- **Darkness.** `zpl_darkness` sends `^MD`, which adjusts the printer's darkness relative to its
  setting and stays in effect for later labels until something changes it.

## Replacing a render service

A common setup today: a small service takes an order id or an image, turns it into a PDF or ZPL of
the right size, and the shop tells printpapi to fetch it with `pdf_uri_post`:

```json
{"printer_id": 2, "type": "pdf_uri_post", "url": "https://render.example/convert",
 "json": {"image": "https://shop.example/orders/4712/label.gif"}}
```

The service exists only to rotate, crop and size the picture for one label stock. With a template
that work moves into printpapi, and the job names the picture directly:

1. On the **Labels** page, create a template for that stock: label size, rotation, crop and where
   the picture sits.
2. Replace the job by:

   ```json
   {"printer_id": 2, "type": "label_uri", "template": "shipping-4x6",
    "url": "https://shop.example/orders/4712/label.gif"}
   ```

Differences to keep in mind:

- `label_uri` fetches with a plain `GET`. There is no POST variant. If the image only exists behind
  a POST, keep the service or fetch it yourself and send it as `label_base64`.
- The output follows the **printer** (PDF or ZPL), so the same job works on a PDF printer and on a
  Zebra without a second code path. A render service usually produces one of the two.
- Input is a GIF, PNG or JPEG. A service that accepts other sources (HTML, SVG, PDF pages) is not
  replaced by this.
- A different label stock is a different template, so a different `template` in the job, not a
  different service.

## Trademarks

DYMO, Zebra, Brother, Rollo, MUNBYN, UPS, DHL, FedEx, GLS and DPD are trademarks of their respective
owners. printpapi is not affiliated with, endorsed by, or sponsored by any of them.
