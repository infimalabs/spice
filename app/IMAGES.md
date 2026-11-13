# Image interaction

These contracts cover image admission, quoting, full-size viewing, and annotation across browser composers and message surfaces.

## Composer images

The chooser keeps admitted image bytes when the complete draft fits one durable
steering record. A larger decodable camera or gallery image is flattened onto
white, resized, and encoded as JPEG within the record budget the draft's other
attachments leave. The shared attachment limits remain the submission
authority: an attachment that does not fit is refused in the composer, never
after Send.

Retained steering attachments and transcript images carry an image-specific
Quote action beside full-size opening. It wears the same quote glyph,
dimensions, edges, colours, and interaction treatment as an ordinary message's
Quote action. The action drafts exactly that image under its admitted name. Its
target names the durable steering or tape source, source ordinal, and
decoded-byte digest; paths, data URLs, tool intent, and browser-generated
diagrams never become quote identity. A sent image quote resolves from the same
durable source and renders an explicit unavailable state if that source no
longer authenticates the named image.

## Message images

A lone tool image renders as a cover card. Two to four such cards in a row from
one speaker, each at most two seconds after the previous, crystallize into one
card at the run's first place; a fifth image, a longer gap, another speaker, or
another card starts a new run. The crystal keeps one card's footprint, one
cell per image in capture order, sized from each image's decoded aspect and the
card's box so none is badly cropped or shrunk to a sliver; cells re-divide as
images load or the box changes. Each cell opens its own image, and hover and
focus mark only that cell. With a lane image open full size, Right Arrow opens
the next newer image and Left Arrow the previous one; each cell is one stop,
navigation never crosses lanes, and the ends do not wrap. Narration, quotes,
receipts, and every other surface still see one message per image.

An authored Markdown image naming an absolute local file renders only when that
lane and occupant previously read exactly that destination successfully as one
captured image. The resident retains the image's identity and exact capture
locus, not a provider-directory binding, and reacquires its bytes from the same
Spice tape generation for live publication, restart and history. A missing,
replaced or altered capture never falls back to another image or provider store;
conflicting reads stay ambiguous. Ordinary links grant no image-read authority.
Explicit external-transcript forensics remains a separate reader.

## Viewer controls

Opening an image retained by a message or task pushes one canonical lightbox
address over that owner’s own route: the message card’s, or the task’s own
address even when a Work list row showed the image. **Copy link** copies it;
Back closes it, Forward and direct navigation reopen it after the owner renders
there, and **Close** returns to its exact owned opener without skipping
unrelated history. Closing returns focus to the image that opened the viewer,
including one its list renders anew. Arrow and swipe steps replace that address
so one Back still closes the viewer. If the address names an unavailable image,
or sits over a route that is not its owner’s own, the app keeps the underlying
route, removes only the image locator, and reports the problem.

Draft images in a composer or a task’s draft editor, and images on a sent
message Chat does not yet retain, are local viewer state: they offer no durable
link, add no history entry, and close by Escape, **Close**, or the backdrop.

In the full-size image viewer, A activates **Annotate** when the image is a
draft image the viewer can annotate, and D activates **Download**: each key
does exactly what its button does, including a refusal, and nothing while the
button is absent or disabled. Held-key repeats, modified keys, and keys typed
into a field never trigger them, and inside the annotation editor A keeps its
arrow-tool meaning.

While an image pasted into a composer is open full size, Right Arrow opens the
next image pasted into that same composer and Left Arrow the previous one, in
the order the composer renders them across its main draft strip and each quote
shard's strip. Navigation never leaves that composer, either end holds without
wrapping, and annotating, IME composition, and Alt, Control, or Command keep
the arrows from stepping. Each image opened this way annotates its own draft
attachment.

On touch, a one-finger swipe steps as the arrows do wherever they would: swipe
left for Right Arrow's image and right for Left Arrow's. It must travel at least
48 pixels and twice as far across as up or down; mouse and pen drags, a gesture
another finger joins, and swipes while annotating never step, while vertical
panning and pinch zoom stay native. While the page is pinch-zoomed, one-finger
travel in any direction pans the zoomed view and never steps.

## Annotation

The image annotation editor closes immediately on Cancel, Escape, or backdrop
while its marks equal the set it opened with; once they differ, those exits
confirm before discarding, and undoing back to the opening set removes the
confirmation. Clicking empty canvas only clears selection. Dirty annotation
work joins the page-unload warning.

Select, Rectangle, Line, Arrow, and Text are also V, R, L, A, and T. Keys 1–6
choose the six visible colours, and G cycles a line or arrow through straight,
elbow, and curve. Arrow keys nudge the selected mark one image pixel, or ten
with Shift; Enter edits a selected label; Delete or Backspace removes the
selection. Command or Control+Z undoes, Shift plus that chord or Command or
Control+Y redoes, and Escape clears a selection before it cancels the editor.

Apply or Command or Control+Enter flattens the marks onto the frozen original
at its intrinsic size. The owner commits that render and its editable shapes
only if the same draft-image revision still exists; a changed or removed image
is refused instead. Apply never asks for discard confirmation.

Annotation modifiers act live: pressing or releasing one mid-gesture updates
the mark without moving the pointer. While drawing a line or arrow, or dragging
one of its ends, Option or Alt routes it as an elbow and Command or Control as a
curve; Shift snaps a straight mark to 45 degrees and mirrors a bent one, so a
curve bows to the other side of its chord and an elbow turns along its other
axis. M mirrors a selected curve or elbow as one undoable edit. The mirrored
bend survives moves, undo, redo, and export. Shift draws or resizes a rectangle
as a square, and Option or Alt draws one from its centre. Dragging a mark with
Shift keeps it to horizontal or vertical travel, and Option or Alt drags a copy
while the original stays in place.
