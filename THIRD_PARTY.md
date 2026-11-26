# Third-party code

## opendeck-t3-code-status

`src/status.rs` is a Rust port of `src/status.ts` from
https://github.com/beastyrabbit/opendeck-t3-code-status at dc27f17. Its license:

```text
MIT License

Copyright (c) 2026 BeastyRabbit

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

## egui default fonts

The settings window uses egui's default fonts, compiled into the exe by `epaint_default_fonts`:
Ubuntu Light (Ubuntu Font Licence 1.0), Hack (MIT), Noto Emoji (SIL OFL 1.1) and emoji-icon-font
(MIT). Their license texts ship with that crate.
