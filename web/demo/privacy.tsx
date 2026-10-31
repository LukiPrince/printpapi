// printpapi — self-hosted PrintNode alternative. Elastic License 2.0 (see LICENSE).
// The demo site's privacy notice, in German for a German operator. Demo build only, reached through
// app/datenschutz/page.demo.tsx. The Cloudflare facts come from cloudflare.com/privacypolicy, the
// customer DPA and developers.cloudflare.com/web-analytics (checked 2026-10-31); change UPDATED with
// any edit.
import Link from "next/link";
import { Logo } from "@/components/brand";

const UPDATED = "31. Oktober 2026";
const EMAIL = "lukaswotzka@icloud.com";

export default function Privacy() {
  return (
    <div className="mx-auto max-w-3xl px-4 py-8 sm:px-6">
      <header className="flex items-center gap-3">
        <Link prefetch={false} href="/" className="flex items-center gap-2.5">
          <Logo className="size-7" />
          <b className="font-mono text-base tracking-tight">printpapi</b>
        </Link>
        <Link prefetch={false} href="/" className="ml-auto text-sm text-muted-foreground hover:text-foreground">
          Zurück zur Demo
        </Link>
      </header>

      <main className="mt-10 space-y-8 leading-relaxed">
        <div>
          <div className="font-mono text-[0.66rem] font-semibold tracking-[0.22em] text-muted-foreground uppercase">
            Stand: {UPDATED}
          </div>
          <h1 className="mt-2 text-3xl font-bold tracking-tight">Datenschutzerklärung</h1>
          <p className="mt-3 text-muted-foreground">
            Diese Seite ist eine kostenlose Demo der Software printpapi. Sie brauchen kein Konto, es gibt
            kein Kontaktformular und keinen Newsletter. Hier steht, welche Daten beim Besuch trotzdem
            anfallen und wer sie verarbeitet.
          </p>
        </div>

        <Section title="Verantwortlicher">
          <p>Verantwortlich im Sinne der Datenschutz-Grundverordnung (DSGVO) ist:</p>
          <p>
            Lukas Wotzka
            <br />
            E-Mail: <a href={`mailto:${EMAIL}`} className="underline underline-offset-2">{EMAIL}</a>
          </p>
        </Section>

        <Section title="Was nur in Ihrem Browser bleibt">
          <p>
            Die Demo läuft vollständig in Ihrem Browser. Was Sie hier ausprobieren, also Testdrucke,
            abgebrochene Aufträge oder neu angelegte Schlüssel, speichert die Seite im sessionStorage
            Ihres Browsers. Dazu kommt ein fester Demo-Schlüssel im localStorage, damit sich das
            Dashboard ohne Anmeldung öffnet. Diese Daten sind erfunden, verlassen Ihr Gerät nicht und
            werden an niemanden übertragen. Der sessionStorage verschwindet, sobald Sie den Tab
            schließen; „Reset“ im Demo-Banner setzt alles sofort zurück.
          </p>
          <p>
            Diese Speicherung ist für die Funktion der Demo erforderlich (§ 25 Abs. 2 Nr. 2 TDDDG).
            Cookies setzt die Seite nicht.
          </p>
        </Section>

        <Section title="Hosting bei Cloudflare">
          <p>
            Die Seite wird über das Netzwerk der Cloudflare, Inc., 101 Townsend St, San Francisco, CA
            94107, USA, ausgeliefert. Damit Ihr Browser die Seite laden kann, verarbeitet Cloudflare
            technisch notwendige Verbindungsdaten, vor allem Ihre IP-Adresse, die aufgerufene Adresse,
            den Zeitpunkt und Angaben zu Ihrem Browser. Cloudflare nutzt diese Daten auch, um die Seite
            vor Angriffen zu schützen. Ich selbst speichere keine Zugriffsprotokolle und sehe keine
            einzelnen Verbindungsdaten.
          </p>
          <p>
            Rechtsgrundlage ist Art. 6 Abs. 1 lit. f DSGVO. Mein berechtigtes Interesse ist, dass die
            Seite sicher und zuverlässig ausgeliefert wird. Cloudflare verarbeitet die Daten als mein
            Auftragsverarbeiter nach Art. 28 DSGVO, auf Grundlage seines Auftragsverarbeitungsvertrags
            (Customer DPA).
          </p>
          <p>
            Cloudflare kann diese Daten in den USA verarbeiten. Cloudflare ist nach dem EU-U.S. Data
            Privacy Framework zertifiziert, für das die EU-Kommission einen Angemessenheitsbeschluss
            erlassen hat (Art. 45 DSGVO). Fällt die Zertifizierung weg, gelten die
            Standardvertragsklauseln der EU-Kommission, die Teil des Auftragsverarbeitungsvertrags sind
            (Art. 46 Abs. 2 lit. c DSGVO).
          </p>
        </Section>

        <Section title="Besucherzählung mit Cloudflare Web Analytics">
          <p>
            Damit ich sehe, wie oft die Demo aufgerufen wird und welche Seiten gelesen werden, nutze ich
            Cloudflare Web Analytics. Ihr Browser lädt dafür ein kleines Skript von
            static.cloudflareinsights.com. Es liest über die Performance-Schnittstelle des Browsers
            Messwerte wie Ladezeiten aus und schickt sie zusammen mit der aufgerufenen Seite an diese
            Website, wo Cloudflare sie auswertet.
          </p>
          <p>
            Nach Angaben von Cloudflare kommt Web Analytics ohne Cookies und ohne localStorage aus,
            erstellt keinen Fingerabdruck Ihres Geräts und verfolgt Sie nicht über andere Websites
            hinweg. Query-Strings, also der Teil einer Adresse nach dem Fragezeichen, werden nicht
            gespeichert. Ich sehe nur zusammengefasste Zahlen wie Aufrufe pro Seite oder Herkunftsländer,
            keine einzelnen Besucher. Cloudflare bewahrt die Rohdaten sieben Tage auf, danach nur noch
            eine verdichtete Stichprobe von etwa zehn Prozent.
          </p>
          <p>
            Rechtsgrundlage ist Art. 6 Abs. 1 lit. f DSGVO. Mein berechtigtes Interesse ist zu wissen, ob
            und wie die Demo genutzt wird. Sie können dem jederzeit widersprechen (siehe unten). Am
            einfachsten geht das mit einem Tracking-Blocker: Die Demo funktioniert auch ohne das Skript.
          </p>
        </Section>

        <Section title="Schriften und Links">
          <p>
            Die Schriften dieser Seite liegen auf demselben Server wie die Seite selbst. Außer dem
            Analytics-Skript lädt Ihr Browser nichts von anderen Anbietern. Links zu GitHub führen auf
            eine fremde Website; dort gilt die Datenschutzerklärung von GitHub.
          </p>
        </Section>

        <Section title="Ihre Rechte">
          <p>
            Sie haben das Recht auf Auskunft über Ihre Daten (Art. 15 DSGVO), auf Berichtigung (Art. 16),
            Löschung (Art. 17), Einschränkung der Verarbeitung (Art. 18) und Datenübertragbarkeit
            (Art. 20). Eine E-Mail an die oben genannte Adresse genügt. Ich selbst speichere allerdings
            keine Daten, mit denen ich Sie erkennen könnte.
          </p>
          <p>
            Soweit Daten auf Grundlage von Art. 6 Abs. 1 lit. f DSGVO verarbeitet werden, können Sie dem
            aus Gründen, die sich aus Ihrer besonderen Situation ergeben, jederzeit widersprechen
            (Art. 21 DSGVO).
          </p>
          <p>
            Außerdem können Sie sich bei einer Datenschutz-Aufsichtsbehörde beschweren, zum Beispiel bei
            der Behörde Ihres Wohnorts (Art. 77 DSGVO).
          </p>
          <p>
            Sie sind nicht verpflichtet, Daten bereitzustellen. Ohne die Verbindungsdaten kann Ihr
            Browser die Seite allerdings nicht laden. Eine automatisierte Entscheidungsfindung oder ein
            Profiling findet nicht statt.
          </p>
        </Section>
      </main>
    </div>
  );
}

function Section({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <section>
      <h2 className="text-lg font-semibold tracking-tight">{title}</h2>
      <div className="mt-2 space-y-3 text-[0.95rem]">{children}</div>
    </section>
  );
}
