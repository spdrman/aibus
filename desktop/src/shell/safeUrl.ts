// External links. CLI output and provider metadata are untrusted (spec 3.3), so a link only becomes
// clickable when it parses as a plain https URL: no other scheme (javascript:, data:, file:, custom
// app schemes), no embedded credentials, no control or bidi characters. Anything else is shown as
// text. The parsed, normalised string is what gets opened, never the raw input.
const UNSAFE_CHARS = /[\u0000-\u001f\u007f-\u009f\u200b-\u200f\u202a-\u202e\u2066-\u2069]/;

export function safeExternalUrl(input: string): string | null {
  if (UNSAFE_CHARS.test(input)) return null;
  let url: URL;
  try {
    url = new URL(input.trim());
  } catch {
    return null;
  }
  if (url.protocol !== "https:" || url.username !== "" || url.password !== "" || url.hostname === "") return null;
  return url.href;
}

// The webview never navigates to an external page itself. Until the host exposes an opener command
// (docs/DEFERRED.md, B1.4), the default asks for a new context with no opener and no referrer.
export function defaultOpener(url: string): void {
  window.open(url, "_blank", "noopener,noreferrer");
}
