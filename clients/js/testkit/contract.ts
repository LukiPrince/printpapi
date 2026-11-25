// A JSON Schema subset checker and a recording fetch. The client tests run the real client through
// this fetch against the real server: every answer must be a status the document declares and match
// its schema (closed objects: a field the server adds fails here until the document lists it), and
// every successful request body must match the request schema.
// ponytail: only the keywords crates/server/openapi.json uses are understood, and walking the
// document throws on any other, so the checker cannot silently accept what it does not check.

// eslint-disable-next-line @typescript-eslint/no-explicit-any
export type Json = any;

const SCHEMA_KEYWORDS = new Set([
  "$ref", "type", "properties", "required", "items", "enum", "oneOf", "additionalProperties",
  "minimum", "maximum", "minLength", "maxLength", "description", "title", "default",
]);

/** Throws on a schema keyword the checker does not understand, anywhere a schema can stand. */
export function assertSupported(doc: Json): void {
  const walk = (s: Json, at: string): void => {
    if (s === null || typeof s !== "object" || Array.isArray(s)) throw new Error(at + ": a schema is an object");
    for (const k of Object.keys(s)) {
      if (!SCHEMA_KEYWORDS.has(k)) throw new Error(at + ": unsupported schema keyword " + k);
    }
    for (const [k, c] of Object.entries(s.properties ?? {})) walk(c, at + "." + k);
    if (s.items) walk(s.items, at + "[]");
    if (s.additionalProperties && typeof s.additionalProperties === "object") walk(s.additionalProperties, at + "{}");
    (s.oneOf ?? []).forEach((c: Json, i: number) => walk(c, at + ".oneOf" + i));
  };
  for (const [name, s] of Object.entries(doc.components.schemas)) walk(s, name);
  for (const [path, item] of Object.entries<Json>(doc.paths)) {
    for (const [method, op] of Object.entries<Json>(item)) {
      const bodies = [op.requestBody, ...Object.values<Json>(op.responses)];
      for (const b of bodies) {
        for (const media of Object.values<Json>(b?.content ?? {})) {
          if (media.schema) walk(media.schema, method + " " + path);
        }
      }
      for (const p of op.parameters ?? []) walk(p.schema, method + " " + path + " " + p.name);
    }
  }
}

function resolve(doc: Json, s: Json): Json {
  if (s.$ref) {
    const target = doc.components.schemas[String(s.$ref).replace("#/components/schemas/", "")];
    if (!target) throw new Error("unresolved " + s.$ref);
    return resolve(doc, target);
  }
  return s;
}

function typeOk(t: string, v: unknown): boolean {
  switch (t) {
    case "string": return typeof v === "string";
    case "integer": return typeof v === "number" && Number.isInteger(v);
    case "number": return typeof v === "number";
    case "boolean": return typeof v === "boolean";
    case "null": return v === null;
    case "array": return Array.isArray(v);
    case "object": return typeof v === "object" && v !== null && !Array.isArray(v);
    default: throw new Error("unsupported type " + t);
  }
}

/** The problems of `value` against `schema` (empty when it matches). */
export function validate(doc: Json, schema: Json, value: unknown, at = "$"): string[] {
  const s = resolve(doc, schema);
  const errs: string[] = [];
  if (s.oneOf) {
    const hits = s.oneOf.filter((c: Json) => validate(doc, c, value, at).length === 0).length;
    if (hits !== 1) errs.push(at + ": matches " + hits + " of " + s.oneOf.length + " alternatives");
    return errs;
  }
  if (s.type !== undefined) {
    const types: string[] = Array.isArray(s.type) ? s.type : [s.type];
    if (!types.some((t) => typeOk(t, value))) {
      return [at + ": expected " + types.join("|") + ", got " + JSON.stringify(value)?.slice(0, 80)];
    }
  }
  if (s.enum && !s.enum.some((e: unknown) => e === value)) {
    errs.push(at + ": " + JSON.stringify(value) + " is not one of " + JSON.stringify(s.enum));
  }
  if (typeof value === "number") {
    if (s.minimum !== undefined && value < s.minimum) errs.push(at + ": below " + s.minimum);
    if (s.maximum !== undefined && value > s.maximum) errs.push(at + ": above " + s.maximum);
  }
  if (typeof value === "string") {
    if (s.minLength !== undefined && value.length < s.minLength) errs.push(at + ": shorter than " + s.minLength);
    if (s.maxLength !== undefined && value.length > s.maxLength) errs.push(at + ": longer than " + s.maxLength);
  }
  if (Array.isArray(value) && s.items) {
    value.forEach((v, i) => errs.push(...validate(doc, s.items, v, at + "[" + i + "]")));
  }
  if (typeof value === "object" && value !== null && !Array.isArray(value)) {
    const obj = value as Record<string, unknown>;
    for (const r of s.required ?? []) if (!(r in obj)) errs.push(at + ": missing " + r);
    for (const [k, v] of Object.entries(obj)) {
      const prop = s.properties?.[k];
      if (prop) errs.push(...validate(doc, prop, v, at + "." + k));
      else if (s.additionalProperties === false) errs.push(at + ": unlisted property " + k);
      else if (s.additionalProperties && typeof s.additionalProperties === "object") {
        errs.push(...validate(doc, s.additionalProperties, v, at + "." + k));
      }
    }
  }
  return errs;
}

export interface Call {
  operationId: string;
  method: string;
  path: string;
  status: number;
}

/** Finds the operation of a request: the path template that matches and its method. */
export function findOperation(doc: Json, method: string, path: string): { operation: Json; template: string } | undefined {
  for (const [template, item] of Object.entries<Json>(doc.paths)) {
    const re = new RegExp("^" + template.replace(/\{[^}]+\}/g, "[^/]+") + "$");
    const op = item[method.toLowerCase()];
    if (op && re.test(path)) return { operation: op, template };
  }
  return undefined;
}

/** A fetch that checks every exchange against the document. */
export class Recorder {
  readonly calls: Call[] = [];
  readonly violations: string[] = [];
  readonly #doc: Json;
  readonly #base: string;
  readonly #inner: typeof fetch;

  constructor(doc: Json, baseUrl: string, inner: typeof fetch = fetch) {
    this.#doc = doc;
    this.#base = new URL(baseUrl).pathname.replace(/\/+$/, "");
    this.#inner = inner;
  }

  readonly fetch: typeof fetch = async (input, init) => {
    const url = new URL(typeof input === "string" ? input : input instanceof URL ? input.href : input.url);
    const method = (init?.method ?? "GET").toUpperCase();
    const path = url.pathname.slice(this.#base.length);
    const res = await this.#inner(input, init);
    try {
      await this.#check(method, path, init?.body, res.clone());
    } catch (e) {
      this.violations.push(method + " " + path + ": " + String(e));
    }
    return res;
  };

  async #check(method: string, path: string, body: BodyInit | null | undefined, res: Response): Promise<void> {
    const found = findOperation(this.#doc, method, path);
    if (!found) {
      this.violations.push(method + " " + path + ": no such operation in the document");
      return;
    }
    const op = found.operation;
    this.calls.push({ operationId: op.operationId, method, path: found.template, status: res.status });
    const declared = op.responses[String(res.status)];
    if (!declared) {
      this.violations.push(method + " " + path + ": status " + res.status + " is not documented");
      return;
    }
    if (res.status >= 200 && res.status < 300 && typeof body === "string" && op.requestBody) {
      const schema = op.requestBody.content?.["application/json"]?.schema;
      if (schema) {
        for (const e of validate(this.#doc, schema, JSON.parse(body))) this.violations.push(method + " " + path + " request " + e);
      }
    }
    const media = Object.keys(declared.content ?? {});
    if (media.length === 0) return;
    const type = (res.headers.get("content-type") ?? "").split(";")[0]?.trim() ?? "";
    if (!media.includes(type)) {
      this.violations.push(method + " " + path + ": content type " + type + " is not one of " + media.join(", "));
      return;
    }
    if (type === "application/json") {
      const schema = declared.content[type].schema;
      for (const e of validate(this.#doc, schema, await res.json())) this.violations.push(method + " " + path + " " + res.status + " " + e);
    }
  }

  assertClean(): void {
    if (this.violations.length > 0) throw new Error("contract violations:\n" + this.violations.join("\n"));
  }
}
