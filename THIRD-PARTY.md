# Third-party content

## App binaries (`.uapp`)

The published site redistributes application binaries built and released by
UNA Watch Ltd from the UNA Watch SDK. They are downloaded unmodified from that
project's public `apps-v*` GitHub releases and republished byte-for-byte; Kira
adds only metadata (a catalogue entry and PNG copies of the icons already
embedded in each binary).

Source project: <https://github.com/UNAWatch/una-sdk>

Those binaries are covered by the SDK's MIT licence:

```
MIT License

Copyright (c) 2026 UNA Watch Ltd

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

## Trademarks

“UNA”, “UNA Watch” and the UNA logo are trademarks of UNA Watch Ltd. Kira is
not affiliated with, endorsed by or sponsored by UNA Watch Ltd.

Kira uses those marks only to state compatibility — a nominative use the SDK's
own [trademark notice](https://github.com/UNAWatch/una-sdk/blob/main/TRADEMARK.md)
permits. Kira does not use the UNA name or logo as its own branding, and the UNA
logo is not redistributed. App icons shown in the catalogue are extracted from
each app's own binary and displayed alongside that app.

## Kira's own code

Everything under `src/`, `site/`, `tools/` and `test/` is MIT licensed — see
[LICENSE](LICENSE). It has no runtime dependencies: the `.uapp` parser, PNG
encoder and CRC-32 implementation are all first-party.
