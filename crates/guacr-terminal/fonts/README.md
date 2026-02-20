# Embedded Fonts

This directory contains fonts used for terminal rendering with fallback support.

The renderer uses a **two-font fallback system** matching guacd's behavior:
1. **Primary:** JetBrains Mono - purpose-built for code/terminal readability
2. **Fallback:** DejaVu Sans Mono - broader Unicode coverage for symbols

## JetBrains Mono (Primary)

- **Version:** 2.304
- **License:** SIL Open Font License 1.1 (see LICENSE-JetBrainsMono.txt)
- **Source:** https://github.com/JetBrains/JetBrainsMono
- **Copyright:** Copyright 2020 The JetBrains Mono Project Authors
- **File size:** ~267KB

### Why JetBrains Mono

- Designed specifically for code/terminal rendering
- Distinct characters: l/1/I, 0/O (reduces misreading)
- Better TrueType hinting at small sizes (common in terminal cells)
- Smaller binary footprint than Noto Sans Mono (~267KB vs ~580KB)

### Coverage

- **Latin** - All European languages, Vietnamese
- **Cyrillic** - Russian, Ukrainian, Serbian, Bulgarian, etc.
- **Greek** - Modern Greek
- **Extended Latin** - Phonetic symbols, diacritics
- **Box Drawing** - U+2500-U+257F (rendered manually for consistency)

## DejaVu Sans Mono (Fallback)

- **Version:** 2.37
- **License:** Bitstream Vera License (see LICENSE-DejaVu.txt)
- **Source:** https://github.com/dejavu-fonts/dejavu-fonts
- **Copyright:** Copyright (c) 2003 Bitstream, Inc. (Vera), DejaVu changes are public domain
- **File size:** ~290KB

### Additional Coverage

DejaVu provides glyphs missing from JetBrains Mono:
- **Mathematical Operators** - U+2200-U+22FF
- **Miscellaneous Technical** - U+2300-U+23FF
- **Geometric Shapes** - U+25A0-U+25FF
- **Arrows** - U+2190-U+21FF
- **Dingbats** - U+2700-U+27BF

This is the same font guacd requires (`dejavu-sans-mono-fonts` package).

## How Fallback Works

```
Character to render
       |
       v
+------------------------------+
| Has glyph in JetBrains Mono? |
+------------------------------+
       | No           | Yes
+------------------+  +--> Use JetBrains Mono
| Has glyph in     |
| DejaVu Sans Mono?|
+------------------+
       | No           | Yes
   Placeholder        +--> Use DejaVu
   rectangle
```

## License Summary

Both fonts can be freely:
- Embedded in software (including commercial)
- Distributed with applications
- Modified (if renamed)
- Sold as part of larger software

**Requirements:**
- Include license files with distribution
- Don't sell fonts standalone

See `LICENSE-JetBrainsMono.txt` and `LICENSE-DejaVu.txt` for full terms.

## Legacy

Noto Sans Mono was previously the primary font. It is no longer included in the
build but its license file is retained for reference. JetBrains Mono was chosen
as a replacement for better terminal readability and smaller binary size.
