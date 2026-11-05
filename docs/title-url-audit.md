# Title and URL leak audit (S2-5)

Contract: ONLY the app id says where the user is. Window titles, tab titles, page titles, URLs
(with reset tokens) and injected text in those places are dropped at the tree reader, then
re-enforced by the pure redactor, then scrubbed in the text that is emitted. D-003 (no window or
page titles) is the user decision behind it.

Layers, in order:
1. **Reader** (`firewall/platform/walk.rs`, shared by Windows UIA and macOS AX): never reads the
   label of Window / TitleBar / WebArea (Document) / Tab, nor of Group/Other at depth <= 2; never
   reads the text value of those roles; drops Tab, nested Window/TitleBar, address-bar-like
   editable nodes, tab strips, bookmark bars and download shelves WITH their subtree (below a
   dropped node only `is_secure` and `children` are asked). `is_secure` is still asked first for
   every node, and secure nodes below dropped chrome are still reported (hidden) so the
   preflight cross-check stays exact.
2. **Redactor** (`firewall/text.rs::redact_tree`, source not trusted): the same drop rules
   (`drops_subtree`), title roles never emitted, then every emitted label / value is sanitised
   and scrubbed (`scrub.rs`) BEFORE the length cap.
3. **Request build** (`llm`): only `RedactedScreenText` can enter; the prompt never interpolates
   screen text, app name or transcript into instructions.
4. **Tripwire** (`core/leakguard.rs`, debug builds only): panics if a registered marker appears in
   a built request. Not a production control (see below).

## Leak paths

| # | Path | Prevented by | Gap / fix in S2-5 |
|---|---|---|---|
| 1 | UIA `Name` of Window / TitleBar (50032, 50037) | mapped to `Window`; walker never calls `label` for Window; `label_is_title` in the redactor | TitleBar CHILDREN (title text) were walked: now nested Window nodes are dropped with their subtree |
| 2 | UIA Name of Document (50030) / AX WebArea | role `WebArea`: label never read (walker), never emitted (redactor); text value never requested | recording-fake test asserts no `label` / `text_value` / `has_value` read |
| 3 | UIA Pane / Group at depth <= 2 | walker never reads label of Group/Other at depth <= 2 | none |
| 4 | Tab strip, TabItem names (other pages' titles) | label never read for `Tab`; | Tab nodes were emitted as unlabeled `tab`: now Tab + subtree dropped. Containers labelled "tab strip / tabs / tab list / tab bar", and toolbars whose name starts with "tab", also dropped with subtree |
| 5 | Address bar / omnibox: the Edit's value | editable values never emitted, only `[has text]` | whole node dropped (the `[has text]` line also told which tab state) |
| 6 | Address bar LABEL ("Address and search bar") and sibling / child nodes carrying the URL | none | editable/TextField/ComboBox whose label matches the lexicon (address, search bar, url, location, omnibox) dropped WITH descendants. Not dropped: labels with email/street/shipping/billing/IP/... ("Email address"). The label of an editable node is read to decide this and discarded with the node; its value text is never requested (only emptiness for nodes that are kept) |
| 7 | Link nodes whose Name IS a URL | none | scrubbed to `link "[link]"` |
| 8 | StaticText / Heading containing a URL, e-mail or token (page content) | none (page content is emitted by design) | URL -> `[link]`, long token -> `[token]`, e-mail -> `[email]` |
| 9 | Document value (the URL) | value never requested for WebArea | test above |
| 10 | "About" dialogs / version strings | ordinary window content; version numbers like `1.2.3` are kept | URLs in them are scrubbed |
| 11 | Bookmarks bar / toolbars | none | containers (Toolbar/Group/List/Other/Table) whose label mentions bookmark(s) dropped with subtree. Other toolbar buttons stay |
| 12 | Status-bar hover URL | page/chrome text | scrubbed (`[link]`); also covered by #8 |
| 13 | Download shelf / bubble file names | none | containers labelled "downloads shelf / bar / bubble / toolbar / panel" dropped with subtree. A file name shown elsewhere (e.g. a file manager) is ordinary content, not a title or URL, and stays |
| 14 | Notification toasts | other process: the walker reads only the foreground window | none |
| 15 | Taskbar / tray tooltips | not in the foreground window tree | none |
| 16 | `app_id` (the `app:` header) | bundle id / exe basename resolved by the platform source; sanitised and capped | a URL-shaped app id (`://`) is withheld (`Withheld(DeniedApp)`); the header is never scrubbed (a bundle id such as `com.example.app` looks like a domain) |
| 17 | `ScreenContext::Text` -> `LlmRequest.screen_text` -> body | only `RedactedScreenText` is accepted (compile_fail doctests) | none |
| 18 | `PreparedRequest` body / preview / `LastRequestDto` / `preview://last/body` | built from the same strings as #17; previewed bytes == sent bytes | e2e test checks body, store body, DTO, events, logs |
| 19 | Tracing logs | only fixed tokens and ids (`turn start`, error codes); no content (L-010, L-015) | e2e test with a log-alive assertion |
| 20 | Events (`turn:*`, `firewall:status`, errors) | fixed tokens / states; only `transcript` and `reply_delta` carry content, and that content is the user's words and the model reply | e2e checks `Rec` events and status |
| 21 | `BlockReason::IncompleteTree { app }` | `app` is the raw bundle id / exe basename from the platform source (not sanitised); it is never rendered, and the spoken sentences are fixed and content free | none |
| 22 | Tray text | fixed `Status: ...` strings | none |
| 23 | Overlay labels / `POINT` labels from the model | model output, sanitised and capped (`sanitize_label`); on the TEXT path the model only saw scrubbed text, so it cannot repeat a title or URL it was never given (this does NOT hold for the image path, see #28) | residual: the model can invent text, and with an image it can read and repeat pixel titles; display only |
| 24 | Audit / diagnostic output | `Debug` of `RawTextNode`, `RawWindowTree`, `RedactedScreenText`, `PreparedRequest` print counts and roles only | none |
| 25 | Approval / preview UI | shows the same strings as #18 (and the preview window shows the exact body) | none |
| 26 | `model_missing` paths in notices | contain the local models directory only (not a title or URL) | listed, fine |
| 27 | Injected instructions in a title ("ignore previous instructions") | titles are never read; in page text the system prompt marks the screen as untrusted data | none |
| 28 | PIXELS / SCREENSHOT: with `VisionMode` Auto or Always (any image sent to the local VLM or to a cloud model) the title bar, tab strip, address bar, bookmarks bar and download shelf are visible as pixels | the tree-level drops and `scrub.rs` do not touch pixels; the secure-field blackout covers password rects only. Current mitigation: an image is only built after a firewall-Clear frame, the VLM is local by default, and a cloud request needs the preview panel (the user sees the image first) | **NOT mitigated otherwise.** Planned in Step 4 (S4-4): black out the rects of the chrome nodes the redactor drops (address bar, tab strip, bookmarks, download shelf) and the window caption / title-bar band (top of the window rect) in the image BEFORE it leaves the firewall. This only adds redactions |

## Screen-text v1.1

Same format as v1 (`app:` header, dense `#id` lines, DFS order, POINT mapping unchanged); the
differences are only removals:
- `[link]` replaces `scheme://...` (any scheme), `www....`, `mailto:`, `file:`, `javascript:`,
  `data:`, `tel:`, `localhost:port/...`, IPv4 (`a.b.c.d[:port][/path]`), IPv6 (bracketed or bare),
  `host.tld/path?query`, `name.tld` for common TLDs (com, net, org, io, dev, app, example, test,
  local, internal, ... the list is in `scrub.rs`), and bare `?a=b` / `&a=b` / `#a=b`
  parameters. A URL ends at whitespace or an unmatched closing bracket; trailing `.,;:!?` stay
  outside the `[link]`.
- `[token]` replaces a run of at least 24 characters of `[A-Za-z0-9_+/=.-]` with at least two of
  upper / lower / digit, or a run of 32+ hex digits.
- `[email]` replaces `local@domain.tld`.
- Scrubbing runs on at most the first 2048 characters of a label / value, before the 120 / 400
  character cap, so a half-cut URL cannot survive. Invisible format characters are first
  removed (not spaced) in a second view; if that view contains a link, it is used, so
  `evil\u{200B}.example` cannot dodge the scrubber.
- Dropped with subtree: `Tab`, nested `Window`/TitleBar, address-bar editables, tab strips,
  bookmark bars, download shelves. `tab` lines no longer appear.

Known false positives (fail toward redaction):
- Hyphen / underscore / dot joined Title-Case words of 24+ characters (`Do-Not-Sell-My-Personal-Information`) become `[token]`.
- Four-part dotted numbers whose parts are 0-255 (`1.2.3.4`) read as IPv4. Three-part versions (`1.2.3`), builds with 4+ digit parts and file names (`main.rs`, `Node.js`) are kept.
- `name.com`-style words in prose (`Amazon.com`) and `Name.Word/` shapes with an alphabetic TLD followed by `/` or `?x` are replaced.
- Labels such as "Address", "Location" or "URL" on editable fields in web FORMS are dropped (not "Email address", "Street address" and the other listed form words).
- Toolbars / groups labelled with "bookmarks", "tab strip" or "downloads shelf" lose their children, also inside page content.
- 32+ digit numbers are treated as hex tokens.
- Not covered: URLs spelled out in words ("evil dot example"), URLs split by real spaces, tokens under 24 characters that are not part of a URL, unassigned Unicode code points.

## Known limitations (not covered, by design or not yet)

- The image path (#28) is not scrubbed: titles, tabs, URLs and bookmarks are readable in a screenshot until S4-4.
- The address-bar / tab-strip / bookmarks / download-shelf lexicons are English only. Tab nodes are dropped by role and editable values are never emitted, so localised address bars and tab strips stay safe, but a localised bookmarks bar or downloads shelf may leak the names of its child links. Manual check: Chrome / Edge in a non-English UI language (`docs/manual-tests.md`).
- Tokens need at least two character classes, so all-uppercase licence keys (`AAAAA-BBBBB-CCCCC-DDDDD-EEEEE`) survive; tokens under 24 characters survive (`sk-ant-api03-abcdefghij`).
- Relative-path tokens (`reset?token=abc`) survive.
- Percent-encoded URLs: the host is removed, the path stays.
- Obfuscated hosts survive: unicode-dot hosts (`evil\u{3002}example`, `evil\u{FF0E}example`), `evil[.]example`, `evil dot example`.

## The debug tripwire is not a control

`core::leakguard` keeps a process-local `TitleLedger` fed ONLY by tests / fakes through
`register_marker` (production never reads a title to register it; the reader never has one). In
debug builds `Buddy::prepare_and_consent` runs `assert_no_known_titles(&prepared)` on the body
(JSON strings, keys decoded), system prompt, transcript, screen text, endpoint and model, with
case / punctuation-insensitive substring matching plus 20-character fragments of longer markers.
A hit panics with the marker's index only. In release builds both functions are empty. It
catches regressions in tests; it does not protect users.
