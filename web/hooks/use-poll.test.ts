import { test } from "node:test";
import assert from "node:assert/strict";

// Just enough DOM for react-dom to mount a component that renders nothing. It has to exist
// before react-dom loads, hence the dynamic imports below.
const noop = () => {};
const element = (tag: string) => ({
  nodeType: 1,
  nodeName: tag.toUpperCase(),
  tagName: tag.toUpperCase(),
  style: {},
  childNodes: [],
  setAttribute: noop,
  removeAttribute: noop,
  appendChild: (c: unknown) => c,
  insertBefore: (c: unknown) => c,
  removeChild: (c: unknown) => c,
  addEventListener: noop,
  removeEventListener: noop,
});
const doc = {
  nodeType: 9,
  createElement: element,
  createTextNode: (t: string) => ({ nodeType: 3, nodeValue: t }),
  addEventListener: noop,
  removeEventListener: noop,
  documentElement: element("html"),
  body: element("body"),
  activeElement: null,
  defaultView: globalThis,
};
Object.assign(globalThis, {
  document: doc,
  window: globalThis,
  HTMLIFrameElement: class {},
  addEventListener: noop,
  removeEventListener: noop,
});

const React = (await import("react")).default;
const { createRoot } = await import("react-dom/client");
const { usePoll } = await import("./use-poll.ts");

const settle = () => new Promise((r) => setTimeout(r, 30));

/** Mounts a component whose fetcher depends on an org id that the test changes, as on Settings. */
function mount(withKey: boolean) {
  const calls: (number | null)[] = [];
  const seen: unknown[] = [];
  let choose: (org: number | null) => void = noop;
  let gate: Promise<void> = Promise.resolve();
  function Probe() {
    const [org, setOrg] = React.useState<number | null>(null);
    choose = setOrg;
    const { data } = usePoll(
      React.useCallback(() => {
        calls.push(org);
        return gate.then(() => (org === null ? null : `org ${org}`));
      }, [org]),
      0,
      true,
      withKey ? org : null,
    );
    seen.push(data);
    return null;
  }
  const container = Object.assign(element("div"), { ownerDocument: doc });
  createRoot(container as unknown as Element).render(React.createElement(Probe));
  return {
    calls,
    seen,
    choose: (org: number | null) => choose(org),
    hold: () => {
      let open = noop;
      gate = new Promise<void>((r) => (open = r));
      return () => open();
    },
  };
}

test("a new key fetches at once, as when Settings learns its org after mounting", async () => {
  const p = mount(true);
  await settle();
  p.choose(5);
  await settle();
  assert.deepEqual(p.calls, [null, 5]);
  assert.equal(p.seen.at(-1), "org 5");
});

test("without a key a changed fetcher is not refetched (interval 0), which is why the key exists", async () => {
  const p = mount(false);
  await settle();
  p.choose(5);
  await settle();
  assert.deepEqual(p.calls, [null]);
  assert.equal(p.seen.at(-1), null);
});

test("a new key drops the old data while the new one loads", async () => {
  const p = mount(true);
  await settle();
  p.choose(5);
  await settle();
  assert.equal(p.seen.at(-1), "org 5");
  const open = p.hold();
  p.choose(7);
  await settle();
  assert.equal(p.seen.at(-1), undefined, "org 5's answer must not show while org 7 loads");
  open();
  await settle();
  assert.equal(p.seen.at(-1), "org 7");
});

test("the same key does not fetch again on a re-render", async () => {
  const p = mount(true);
  await settle();
  p.choose(5);
  await settle();
  p.choose(5);
  await settle();
  assert.deepEqual(p.calls, [null, 5]);
});
