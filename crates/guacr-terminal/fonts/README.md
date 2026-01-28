# Embedded Fonts

This directory contains fonts used for terminal rendering with fallback support.

The renderer uses a **two-font fallback system** matching guacd's behavior:
1. **Primary:** Noto Sans Mono - optimized for common text
2. **Fallback:** DejaVu Sans Mono - broader Unicode coverage for symbols

## Noto Sans Mono (Primary)

- **Version:** 2.014
- **License:** SIL Open Font License 1.1 (see LICENSE-Noto.txt)
- **Source:** https://github.com/notofonts/latin-greek-cyrillic
- **Copyright:** Copyright 2022 The Noto Project Authors
- **File size:** ~580KB

### Coverage

- **Latin** - All European languages, Vietnamese
- **Cyrillic** - Russian, Ukrainian, Serbian, Bulgarian, etc.
- **Greek** - Ancient and Modern Greek
- **Extended Latin** - Phonetic symbols, diacritics
- **Box Drawing** - U+2500-U+257F (rendered manually for consistency)

## DejaVu Sans Mono (Fallback)

- **Version:** 2.37
- **License:** Bitstream Vera License (see LICENSE-DejaVu.txt)
- **Source:** https://github.com/dejavu-fonts/dejavu-fonts
- **Copyright:** Copyright (c) 2003 Bitstream, Inc. (Vera), DejaVu changes are public domain
- **File size:** ~290KB

### Additional Coverage

DejaVu provides glyphs missing from Noto Sans Mono:
- **Mathematical Operators** - U+2200-U+22FF (∀, ∃, ∈, ∉, etc.)
- **Miscellaneous Technical** - U+2300-U+23FF (⌘, ⏵, ⎿, etc.)
- **Geometric Shapes** - U+25A0-U+25FF (▲, ▼, ◀, ▶, etc.)
- **Arrows** - U+2190-U+21FF
- **Dingbats** - U+2700-U+27BF

This is the same font guacd requires (`dejavu-sans-mono-fonts` package).

## How Fallback Works

```
Character to render
       ↓
┌──────────────────────────────┐
│ Has glyph in Noto Sans Mono? │
└──────────────────────────────┘
       ↓ No           ↓ Yes
┌──────────────────┐  └──→ Use Noto
│ Has glyph in     │
│ DejaVu Sans Mono?│
└──────────────────┘
       ↓ No           ↓ Yes
   Placeholder        └──→ Use DejaVu
   rectangle
```

## License Summary

Both fonts can be freely:
- ✅ Embedded in software (including commercial)
- ✅ Distributed with applications
- ✅ Modified (if renamed)
- ✅ Sold as part of larger software

**Requirements:**
- Include license files with distribution
- Don't sell fonts standalone

See `LICENSE-Noto.txt` and `LICENSE-DejaVu.txt` for full terms.
