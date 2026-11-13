# Rack

The browser reads packaged documentation through `rack::read`, one typed,
operator-read command over the existing app WebSocket. This is not a generic
resource or filesystem endpoint: it cannot accept a URL, extraction destination,
write, executable content, or path outside the linked rack's `docs` directory.

## Identity and paths

Every answer identifies the exact linked archive digest and, when the journal
admits it, that fact's short code. A later request may repeat the short code as a
generation pin; a mismatch refuses rather than reading another generation.

The empty path names the documentation root. Every nonempty path is relative to
that root and must already be normalized. Absolute paths, `.` and `..`, repeated
separators, backslashes, control characters, and paths over 4 KiB refuse.
Responses and diagnostics never disclose extraction or host filesystem paths.

## Content admission

Directory answers list immediate children in stable path order, capped at 1,024
entries. Each row names its relative path, kind, optional declared content type,
and uncompressed byte size.

Member answers admit UTF-8 Markdown or plain text through 512 KiB. PNG, JPEG,
GIF, and WebP are admitted only when observed bytes match the declared type and
the canonical image data URL fits the shared media and encoded-data budgets.
Unknown types, executable text such as HTML or SVG, invalid UTF-8, mismatched
image bytes, missing members, and every over-bound value refuse without returning
content bytes.

## Format and linking

Every appliance executable links one spice rack: `app/` members copied
unchanged from `G.zip`, the root members `SKILL.md`, `spice.toml`, and
`build.json`, and `docs/...`. The standalone [`rack`](../rack/src/lib.rs)
crate reads racks as plain STORE ZIP archives. Its reader uses only std and
zip; Cargo features add `write`, `link` (emit and extract the linked object),
and `linked` (read the running executable's rack). The writer sorts members by
name and stores exact bytes under ZIP STORE at 1980-01-01, so the bytes are a
pure function of the members. Admission parses the central directory once.
Timestamps and comments do not affect rack semantics; metadata that changes
structure, encryption, method, sizes, or data ranges is validated. Admission
refuses malformed or multi-disk structure, repeated or unnormalized member
paths, local-header disagreement, encryption, a method other than STORE,
unequal stored sizes, and unavailable member ranges.
Lookup borrows stored bytes. Spice's convention lives in
[`serve/src/canvas/gzip.rs`](../serve/src/canvas/gzip.rs): every member is
one deterministic gzip stream (fixed header, best level) named by its logical
path plus one `.gz` (`foo.gz` is stored as `foo.gz.gz`), so a generic ZIP tool
extracts named gzip files and app members are served as-is under
`Content-Encoding: gzip`; a name without the suffix or a stream that is not
gzip is refused, and a member inflates on request, checked against its
trailer. Integrity rests on the archive digest.
[`serve/src/canvas.rs`](../serve/src/canvas.rs) reads G from the app shell,
[`xtask/src/app.rs`](../xtask/src/app.rs) stages `G.zip`, rewriting one that
no longer admits, and [`xtask/src/app/rack.rs`](../xtask/src/app/rack.rs)
assembles the rack.

A resident reads rack members as `spice://SKILL.md` and `spice://DIR/PATH` with
the usual line selectors; a directory lists its members. At startup an
installed executable admits its rack through one `rack::admitted` fact naming
the archive digest and app generation, appended once per distinct archive. That
fact's sequence code is the short code a rack URL names: `spice://DIR/PATH@CODE`
answers what `spice://DIR/PATH` answers when CODE is the linked rack's, and
another code is refused naming it. No URL names a generation by digest or UUID;
an unadmitted rack refuses every `@` segment. Content type comes from the path
extension through Spice's one table, `serve::content_type`, never sniffed.
Assembly refuses an extension the table does not name. `rack::read` lists any
such member as unsupported and refuses opening it; the `spice://` reader
likewise refuses the member itself. Text members answer inline; any other
answers one
line naming its type, size, digest, short code, and a file under
`.spice/cache/rack/G/` named by that digest, extracted once on first read. `spice
lane purge` retires every generation directory but the linked one. `spice
resource read URL` prints exactly what a lane's host frame carries.

The `linked` feature declares the two symbols the relocatable object defines:
`spicezip` at the rack's first byte and `spicezip_len` at its length, so the
process reads its rack as a static slice with no file offset or image parse.
`rack::link` locates that rack in final Mach-O, ELF, and PE32+ images:
`__spicezip` on Mach-O and `spicezip` on ELF and PE32+, exactly PE's eight-byte
section-name limit.
