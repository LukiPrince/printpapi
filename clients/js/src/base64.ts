// printpapi client: bytes to base64 without Buffer, so the same code runs in a browser.

/** Standard base64 of `bytes` (what the server's *_base64 fields take). */
export function toBase64(bytes: Uint8Array): string {
  let s = "";
  for (let i = 0; i < bytes.length; i += 0x8000) {
    s += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  }
  return btoa(s);
}
