# Third-party notices

## Diagram renderer

The browser app draws Markdown diagrams with Mermaid 12.0.0, copied byte for
byte from its npm release `dist/mermaid.esm.min.mjs` and every
`dist/chunks/mermaid.esm.min/*.mjs` chunk, with source maps omitted. The
files live in `app/renderer`, and the app build publishes every file there
into each app generation. The provenance index
[`app/renderer/renderer-000.txt`](../app/renderer/renderer-000.txt) names each
of the 55 packages that bundle carries: Mermaid and every dependency it
incorporates, with version, registry tarball, integrity, declared license, and
the files holding that package's license and notice text verbatim. Packages
with byte-identical notice text share one file. Those notice files ship in the
same app generation as the code they cover.

One bundled package, elkjs 0.9.3, is licensed under the Eclipse Public License
2.0, shipped verbatim as `app/renderer/renderer-041.txt`. It is distributed
here only inside that byte-for-byte Mermaid bundle. The source code of elkjs
is available under the Eclipse Public License 2.0 from
<https://github.com/kieler/elkjs>, at the commit its npm release records,
`a8304cf79fde75bc2ab1a89d28320f53f8637436`; the exact published package is
<https://registry.npmjs.org/elkjs/-/elkjs-0.9.3.tgz>.

khroma 2.1.0 declares no license field in its package metadata, so the index
records none for it. It ships under the MIT License, whose text is its own
`license` file, shipped verbatim as `app/renderer/renderer-047.txt`.

## English stop-word vocabulary

The shared matching vocabulary in `board/src/similarity.rs` incorporates the
318-word English stop list from scikit-learn's
[`sklearn/feature_extraction/_stop_words.py`](https://github.com/scikit-learn/scikit-learn/blob/main/sklearn/feature_extraction/_stop_words.py).
The source attributes this list to the Glasgow Information Retrieval Group.
The copied source has SHA-256
`6447f01191d2345af489dda93dd06566af84f63e9515033c4bcea0b9bccec1ea`.
The vocabulary is compiled into Spice; neither Python nor a network fetch is
required to use it.

The compiled vocabulary is the exact 318-word reference list. Reference
spellings are preserved, including entries shorter than the tokenizer's
admission minimum.

The following upstream license applies to the incorporated reference list.

### BSD 3-Clause License

Copyright (c) 2007-2026 The scikit-learn developers.
All rights reserved.

Redistribution and use in source and binary forms, with or without
modification, are permitted provided that the following conditions are met:

* Redistributions of source code must retain the above copyright notice, this
  list of conditions and the following disclaimer.

* Redistributions in binary form must reproduce the above copyright notice,
  this list of conditions and the following disclaimer in the documentation
  and/or other materials provided with the distribution.

* Neither the name of the copyright holder nor the names of its
  contributors may be used to endorse or promote products derived from
  this software without specific prior written permission.

THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS"
AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE
IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE
DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR CONTRIBUTORS BE LIABLE
FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL
DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR
SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER
CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY,
OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE
OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
