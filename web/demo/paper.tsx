// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
// What a virtual printer puts out: label, packing slip / invoice, receipt, test sheet, or a file card.
// Port of the mockup's paperFor(); the bytes the pipeline "sent" and this picture agree via data.ts.
import { Archive } from "lucide-react";
import { SHOP, eur, orderData, receiptData, rng } from "./data.ts";
import type { DemoJob, DemoState } from "./seed.ts";
import styles from "./demo.module.css";

function Barcode({ seed, w = 140, h = 40, className }: { seed: number; w?: number; h?: number; className?: string }) {
  const r = rng(seed);
  const bars: { x: number; w: number }[] = [];
  for (let x = 0; x < w - 4; ) {
    const bw = 1 + Math.floor(r() * 3);
    bars.push({ x, w: bw });
    x += bw + 1 + Math.floor(r() * 3);
  }
  return (
    <svg className={`${styles.bc} ${className ?? ""}`} viewBox={`0 0 ${w} ${h}`} preserveAspectRatio="none" aria-hidden="true">
      <g fill="currentColor">
        {bars.map((b) => (
          <rect key={b.x} x={b.x} width={b.w} height={h} />
        ))}
      </g>
    </svg>
  );
}

export function Paper({ job, state, feeding }: { job: DemoJob; state: DemoState; feeding?: boolean }) {
  const p = state.printers.find((x) => x.id === job.printer_id);
  const agent = p ? state.agents.find((a) => a.id === p.agent_id) : undefined;
  const feed = feeding ? ` ${styles.feeding}` : "";
  const ref = job.ref ?? job.id;

  if (p?.paper === "file") {
    const file = job.file ?? `${(job.title || "job").replace(/[^\w.-]+/g, "-").toLowerCase()}.pdf`;
    return (
      <div className={styles.fileout}>
        <Archive aria-hidden="true" />
        <div>
          <b>Saved, not printed</b>
          <br />
          <code>/srv/print-archive/2026-10/{file}</code>
        </div>
      </div>
    );
  }

  if (job.kind === "label" && job.ref !== null) {
    const o = orderData(ref);
    const [name, street, city, cc] = o.person;
    return (
      <div className={`${styles.paper} ${styles.label}${feed}`} role="img" aria-label={`Shipping label for ${name}`}>
        <div className={styles.sec}>
          <span className={styles.k}>FROM</span>
          {SHOP.name}
          <br />
          {SHOP.street}, {SHOP.city}
        </div>
        <div className={`${styles.sec} ${styles.to}`}>
          <span className={styles.k}>SHIP TO</span>
          <b>{name}</b>
          {street}
          <br />
          {city} · {cc}
        </div>
        <div className={styles.sec}>
          <Barcode seed={ref} />
          <div className={styles.trk}>{o.tracking}</div>
        </div>
        <div className={styles.meta}>
          <span>ORDER #{ref}</span>
          <span>{o.kg} KG</span>
          <span>1/1</span>
        </div>
      </div>
    );
  }

  if ((job.kind === "slip" || job.kind === "invoice") && job.ref !== null) {
    const o = orderData(job.kind === "invoice" ? 10400 + ref : ref);
    const [name, street, city] = o.person;
    const head = job.kind === "invoice" ? `Invoice RE-2026-0${ref}` : "Packing slip";
    return (
      <div className={`${styles.paper} ${styles.page}${feed}`} role="img" aria-label={head}>
        <div className={styles.ph}>
          <b>{SHOP.name}</b>
          <span>{head}</span>
        </div>
        <div>
          Order #{o.n} · 31 Oct 2026
          <br />
          <b>{name}</b> · {street} · {city}
        </div>
        <table>
          <tbody>
            <tr>
              <th>Qty</th>
              <th>Item</th>
              <th>Total</th>
            </tr>
            {o.lines.map((l) => (
              <tr key={l.sku}>
                <td>{l.qty}</td>
                <td>
                  {l.name}
                  <br />
                  <span className={styles.sku}>{l.sku}</span>
                </td>
                <td>{eur(l.total)}</td>
              </tr>
            ))}
            {o.ship > 0 && (
              <tr>
                <td />
                <td>Shipping</td>
                <td>{eur(o.ship)}</td>
              </tr>
            )}
          </tbody>
        </table>
        <div className={styles.tot}>
          <span>Total</span>
          <span>{eur(o.total)}</span>
        </div>
        {o.note && <div>Note: {o.note}</div>}
        <div className={styles.foot}>
          <span>printpapi packing slip</span>
          <span>page 1/1</span>
        </div>
      </div>
    );
  }

  if (job.kind === "receipt" && job.ref !== null) {
    const rc = receiptData(ref);
    return (
      <div className={`${styles.paper} ${styles.rcpt}${feed}`} role="img" aria-label={`Receipt A-${rc.n}`}>
        <b className={styles.c}>NORDLICHT COFFEE</b>
        <span className={styles.c}>{SHOP.street} · Hamburg</span>
        <hr />
        {rc.items.map(([n, v]) => (
          <div key={n} className={styles.l}>
            <span>{n}</span>
            <span>{v.toFixed(2)}</span>
          </div>
        ))}
        <hr />
        <div className={styles.l}>
          <b>TOTAL</b>
          <b>{rc.total.toFixed(2)}</b>
        </div>
        <span className={styles.c}>#A-{rc.n} · 31.10.26 {rc.time}</span>
        <Barcode seed={rc.n * 3} w={100} h={22} />
      </div>
    );
  }

  // anything else is a test print or a file the visitor sent
  if (p?.can_pdf) {
    const fname = job.file ?? "printpapi test page";
    const test = job.kind === "test";
    return (
      <div className={`${styles.paper} ${styles.page}${feed}`} role="img" aria-label={fname}>
        <div className={styles.ph}>
          <b>{job.title || fname}</b>
          <span>{test ? "test" : "pdf"}</span>
        </div>
        <div className={styles.big}>{test ? "printpapi test page" : fname}</div>
        <div className={styles.sub}>
          {p.name} · {agent?.name}
          {job.copies > 1 ? ` · ${job.copies} copies` : ""}
        </div>
        <div className={styles.foot}>
          <span>job #{job.id}</span>
          <span>page 1/1</span>
        </div>
      </div>
    );
  }
  if (p?.paper === "receipt") {
    return (
      <div className={`${styles.paper} ${styles.rcpt}${feed}`} role="img" aria-label="Test receipt">
        <b className={styles.c}>printpapi test</b>
        <hr />
        <span className={styles.c}>{p.name}</span>
        <span className={styles.c}>job #{job.id}</span>
        <Barcode seed={job.id} w={100} h={22} />
      </div>
    );
  }
  return (
    <div className={`${styles.paper} ${styles.label} ${styles.test}${feed}`} role="img" aria-label="Test label">
      <b>printpapi test</b>
      <span>{job.file ?? p?.name}</span>
      <Barcode seed={job.id} w={120} />
      <span>job #{job.id}</span>
    </div>
  );
}

export { styles as paperStyles };
