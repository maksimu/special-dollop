//! 3270 Screen Buffer Model.
//!
//! Implements the 24x80 (or alternate size) character grid used by IBM 3270
//! terminals. The screen is a linear buffer of cells, each containing an
//! EBCDIC character and display attributes. Fields are defined by Start Field
//! (SF) orders and span from one field attribute to the next.
//!
//! The buffer address wraps around: after the last position, it continues
//! from position 0.

use crate::datastream::{
    Aid, Color3270, DataStream, DataStreamItem, ExtendedAttribute, FieldAttribute, Highlighting,
    Intensity, Order, Wcc, WriteCommand,
};
use crate::ebcdic::{self, CodePage, EBCDIC_SPACE};

/// Display attributes for a single cell, including extended color and
/// highlighting information.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CellAttribute {
    /// Foreground color (from SA or SFE orders).
    pub foreground: Color3270,
    /// Background color (from SA or SFE orders).
    pub background: Color3270,
    /// Extended highlighting mode.
    pub highlight: Highlight3270,
    /// If this cell is a field attribute position, the field attribute byte.
    /// Field attribute positions display as blanks on a real 3270 terminal.
    pub field_attribute: Option<FieldAttribute>,
}

/// Highlight modes for screen cells (mirrors the extended highlighting attribute).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Highlight3270 {
    Normal,
    Blink,
    ReverseVideo,
    Underscore,
    Intensified,
}

impl Default for CellAttribute {
    fn default() -> Self {
        CellAttribute {
            foreground: Color3270::Default,
            background: Color3270::Default,
            highlight: Highlight3270::Normal,
            field_attribute: None,
        }
    }
}

impl From<Highlighting> for Highlight3270 {
    fn from(h: Highlighting) -> Self {
        match h {
            Highlighting::Default => Highlight3270::Normal,
            Highlighting::Blink => Highlight3270::Blink,
            Highlighting::ReverseVideo => Highlight3270::ReverseVideo,
            Highlighting::Underscore => Highlight3270::Underscore,
            Highlighting::Intensified => Highlight3270::Intensified,
        }
    }
}

/// A single cell in the 3270 screen buffer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cell {
    /// The Unicode character displayed in this cell.
    pub character: char,
    /// Display attributes for this cell.
    pub attribute: CellAttribute,
}

impl Default for Cell {
    fn default() -> Self {
        Cell {
            character: ' ',
            attribute: CellAttribute::default(),
        }
    }
}

/// A field on the 3270 screen, defined by a Start Field (SF) order.
///
/// Fields span from the position after a field attribute to just before
/// the next field attribute (or wrapping around the buffer).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    /// Buffer position of the field attribute byte.
    pub start: u16,
    /// Buffer position of the last data cell in this field (inclusive).
    /// If the field wraps, end < start.
    pub end: u16,
    /// The field attribute.
    pub attribute: FieldAttribute,
    /// Whether this field has been modified by the user since the last
    /// reset-MDT command.
    pub modified: bool,
}

/// The 3270 screen buffer.
///
/// Models the full terminal display as a linear array of cells. Standard
/// screen sizes are 24x80 (Model 2), 32x80 (Model 3), 43x80 (Model 4),
/// and 27x132 (Model 5).
pub struct ScreenBuffer {
    /// Number of rows.
    rows: u16,
    /// Number of columns.
    cols: u16,
    /// Linear buffer of cells (rows * cols entries).
    buffer: Vec<Cell>,
    /// Current buffer address (cursor position for data stream processing).
    cursor_position: u16,
    /// The code page used for EBCDIC decoding.
    code_page: CodePage,
    /// Current Set Attribute (SA) state - foreground color.
    current_fg: Color3270,
    /// Current Set Attribute (SA) state - background color.
    current_bg: Color3270,
    /// Current Set Attribute (SA) state - highlighting.
    current_highlight: Highlight3270,
}

impl ScreenBuffer {
    /// Create a new screen buffer with the given dimensions.
    ///
    /// Common sizes: 24x80 (standard), 32x80, 43x80, 27x132.
    pub fn new(rows: u16, cols: u16) -> Self {
        let size = rows as usize * cols as usize;
        ScreenBuffer {
            rows,
            cols,
            buffer: vec![Cell::default(); size],
            cursor_position: 0,
            code_page: CodePage::Cp037,
            current_fg: Color3270::Default,
            current_bg: Color3270::Default,
            current_highlight: Highlight3270::Normal,
        }
    }

    /// Create a new screen buffer with a specified code page.
    pub fn with_code_page(rows: u16, cols: u16, code_page: CodePage) -> Self {
        let mut screen = Self::new(rows, cols);
        screen.code_page = code_page;
        screen
    }

    /// Total number of buffer positions.
    pub fn size(&self) -> u16 {
        self.rows * self.cols
    }

    /// Number of rows.
    pub fn rows(&self) -> u16 {
        self.rows
    }

    /// Number of columns.
    pub fn cols(&self) -> u16 {
        self.cols
    }

    /// Get the current cursor position as a linear buffer address.
    pub fn cursor_pos(&self) -> u16 {
        self.cursor_position
    }

    /// Get the cursor position as (row, col).
    pub fn cursor_row_col(&self) -> (u16, u16) {
        (
            self.cursor_position / self.cols,
            self.cursor_position % self.cols,
        )
    }

    /// Get a reference to the cell at the given (row, col).
    ///
    /// Returns None if row or col is out of range.
    pub fn get_cell(&self, row: u16, col: u16) -> Option<&Cell> {
        if row < self.rows && col < self.cols {
            let idx = (row as usize) * (self.cols as usize) + (col as usize);
            Some(&self.buffer[idx])
        } else {
            None
        }
    }

    /// Get a reference to the cell at a linear buffer address.
    pub fn get_cell_at(&self, pos: u16) -> &Cell {
        &self.buffer[pos as usize % self.buffer.len()]
    }

    /// Clear the entire screen buffer to spaces with default attributes.
    pub fn clear(&mut self) {
        for cell in &mut self.buffer {
            *cell = Cell::default();
        }
        self.cursor_position = 0;
        self.reset_sa_state();
    }

    /// Reset the current SA (Set Attribute) state to defaults.
    fn reset_sa_state(&mut self) {
        self.current_fg = Color3270::Default;
        self.current_bg = Color3270::Default;
        self.current_highlight = Highlight3270::Normal;
    }

    /// Advance the buffer address by one position, wrapping at the end.
    fn advance(&mut self) {
        self.cursor_position = (self.cursor_position + 1) % self.size();
    }

    /// Write a character at the current buffer position and advance.
    fn write_char(&mut self, ebcdic_byte: u8) {
        let ch = ebcdic::ebcdic_to_unicode(ebcdic_byte, self.code_page);
        let pos = self.cursor_position as usize;
        self.buffer[pos].character = ch;
        self.buffer[pos].attribute.foreground = self.current_fg;
        self.buffer[pos].attribute.background = self.current_bg;
        self.buffer[pos].attribute.highlight = self.current_highlight;
        // Preserve field_attribute if this position has one
        self.advance();
    }

    /// Process a WCC (Write Control Character).
    fn apply_wcc(&mut self, wcc: &Wcc) {
        if wcc.reset_mdt {
            // Reset the MDT bit on all field attribute positions
            for cell in &mut self.buffer {
                if let Some(ref mut fa) = cell.attribute.field_attribute {
                    fa.modified = false;
                }
            }
        }
        // restore_keyboard and alarm are handled by the caller (UI layer)
    }

    /// Apply a complete data stream to the screen buffer.
    ///
    /// Processes the write command, WCC, and all orders/characters in sequence.
    pub fn apply_data_stream(&mut self, stream: &DataStream) {
        // Handle erase commands
        match stream.command {
            WriteCommand::EraseWrite | WriteCommand::EraseWriteAlternate => {
                self.clear();
            }
            WriteCommand::Write => {
                // No erase, write to current buffer content
            }
            WriteCommand::WriteStructuredField => {
                // WSF data is handled differently (structured fields)
                // For now, store raw data as characters (basic support)
            }
        }

        // Apply WCC
        self.apply_wcc(&stream.wcc);

        // Reset SA state at the start of a data stream
        self.reset_sa_state();

        // Process each item in the data stream
        for item in &stream.orders {
            match item {
                DataStreamItem::Order(order) => self.apply_order(order),
                DataStreamItem::Character(byte) => self.write_char(*byte),
            }
        }
    }

    /// Apply a single order to the screen buffer.
    fn apply_order(&mut self, order: &Order) {
        match order {
            Order::Sba(addr) => {
                self.cursor_position = *addr % self.size();
            }
            Order::Sf(attr) => {
                // Place the field attribute at the current position.
                // The field attribute position displays as a blank.
                let pos = self.cursor_position as usize;
                self.buffer[pos].character = ' ';
                self.buffer[pos].attribute.field_attribute = Some(*attr);
                self.buffer[pos].attribute.foreground = Color3270::Default;
                self.buffer[pos].attribute.background = Color3270::Default;
                self.buffer[pos].attribute.highlight = Highlight3270::Normal;
                self.advance();
            }
            Order::Sfe(attrs) => {
                // Start Field Extended: set field attribute with extended attrs
                let pos = self.cursor_position as usize;
                self.buffer[pos].character = ' ';
                let mut fa = FieldAttribute {
                    protected: false,
                    numeric: false,
                    intensity: Intensity::Normal,
                    modified: false,
                };
                for ea in attrs {
                    match ea {
                        ExtendedAttribute::FieldAttribute(a) => fa = *a,
                        ExtendedAttribute::ForegroundColor(c) => {
                            self.buffer[pos].attribute.foreground = *c;
                        }
                        ExtendedAttribute::BackgroundColor(c) => {
                            self.buffer[pos].attribute.background = *c;
                        }
                        ExtendedAttribute::Highlighting(h) => {
                            self.buffer[pos].attribute.highlight = (*h).into();
                        }
                        _ => {}
                    }
                }
                self.buffer[pos].attribute.field_attribute = Some(fa);
                self.advance();
            }
            Order::Sa(attr) => {
                // Set Attribute: change the current character attribute state
                // (does not create a field, does not occupy a buffer position)
                match attr {
                    ExtendedAttribute::ForegroundColor(c) => self.current_fg = *c,
                    ExtendedAttribute::BackgroundColor(c) => self.current_bg = *c,
                    ExtendedAttribute::Highlighting(h) => {
                        self.current_highlight = (*h).into();
                    }
                    _ => {}
                }
            }
            Order::Ic => {
                // Insert Cursor: the cursor position is already at the right place.
                // The cursor_position is what the terminal will display.
                // (No buffer modification needed - cursor_position is already set.)
            }
            Order::Pt => {
                // Program Tab: advance to the next unprotected field.
                self.tab_forward();
            }
            Order::Ra(addr, ch) => {
                // Repeat to Address: fill from current position to addr with ch
                let target = *addr % self.size();
                loop {
                    if self.cursor_position == target {
                        break;
                    }
                    self.write_char(*ch);
                }
            }
            Order::Eua(addr) => {
                // Erase Unprotected to Address: clear unprotected cells to addr
                let target = *addr % self.size();
                loop {
                    if self.cursor_position == target {
                        break;
                    }
                    let pos = self.cursor_position as usize;
                    // Only clear if the cell is in an unprotected field
                    if !self.is_protected(self.cursor_position) {
                        self.buffer[pos].character = ' ';
                    }
                    self.advance();
                }
            }
            Order::Mf(attrs) => {
                // Modify Field: modify attributes of the field at current position
                let pos = self.cursor_position as usize;
                for ea in attrs {
                    match ea {
                        ExtendedAttribute::FieldAttribute(a) => {
                            self.buffer[pos].attribute.field_attribute = Some(*a);
                        }
                        ExtendedAttribute::ForegroundColor(c) => {
                            self.buffer[pos].attribute.foreground = *c;
                        }
                        ExtendedAttribute::BackgroundColor(c) => {
                            self.buffer[pos].attribute.background = *c;
                        }
                        ExtendedAttribute::Highlighting(h) => {
                            self.buffer[pos].attribute.highlight = (*h).into();
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    /// Check if a buffer position is in a protected field.
    fn is_protected(&self, pos: u16) -> bool {
        // Walk backward from pos to find the governing field attribute
        let size = self.size();
        let mut check = pos;
        for _ in 0..size {
            if let Some(fa) = &self.buffer[check as usize].attribute.field_attribute {
                return fa.protected;
            }
            if check == 0 {
                check = size - 1;
            } else {
                check -= 1;
            }
        }
        // No field attributes found: default is unprotected
        false
    }

    /// Get all fields currently defined on the screen.
    ///
    /// Returns fields in screen order. Each field starts at the position
    /// after the field attribute byte and extends to just before the next
    /// field attribute byte.
    pub fn get_fields(&self) -> Vec<Field> {
        let size = self.size();
        let mut fa_positions: Vec<(u16, FieldAttribute)> = Vec::new();

        // Collect all field attribute positions
        for i in 0..size {
            if let Some(fa) = &self.buffer[i as usize].attribute.field_attribute {
                fa_positions.push((i, *fa));
            }
        }

        if fa_positions.is_empty() {
            return Vec::new();
        }

        let mut fields = Vec::with_capacity(fa_positions.len());
        for (idx, &(start_pos, ref attr)) in fa_positions.iter().enumerate() {
            let next_idx = (idx + 1) % fa_positions.len();
            let next_start = fa_positions[next_idx].0;

            // The field's data area starts at start_pos + 1 (after the attribute byte).
            // It ends just before the next field attribute.
            let end = if next_start == 0 {
                size - 1
            } else {
                next_start - 1
            };

            fields.push(Field {
                start: start_pos,
                end,
                attribute: *attr,
                modified: attr.modified,
            });
        }

        fields
    }

    /// Get the field containing the given buffer position, if any.
    pub fn get_field_at(&self, pos: u16) -> Option<Field> {
        let fields = self.get_fields();
        for field in &fields {
            if field.start <= field.end {
                // Non-wrapping field
                if pos >= field.start && pos <= field.end {
                    return Some(field.clone());
                }
            } else {
                // Wrapping field
                if pos >= field.start || pos <= field.end {
                    return Some(field.clone());
                }
            }
        }
        None
    }

    /// Move the cursor to the next unprotected field.
    ///
    /// Scans forward from the current cursor position looking for an
    /// unprotected field. If found, positions the cursor at the first
    /// data position of that field.
    pub fn tab_forward(&mut self) {
        let size = self.size();
        let start = self.cursor_position;
        let mut pos = (start + 1) % size;

        for _ in 0..size {
            if let Some(fa) = &self.buffer[pos as usize].attribute.field_attribute {
                if !fa.protected {
                    // Found an unprotected field; position cursor at first data cell
                    self.cursor_position = (pos + 1) % size;
                    return;
                }
            }
            pos = (pos + 1) % size;
        }
        // No unprotected field found; cursor stays put
    }

    /// Move the cursor to the previous unprotected field.
    ///
    /// Scans backward from the current cursor position.
    pub fn tab_backward(&mut self) {
        let size = self.size();
        let start = self.cursor_position;

        // First, find the field attribute that governs the current position
        // or the one before it, then continue scanning backward.
        let mut pos = if start == 0 { size - 1 } else { start - 1 };

        for _ in 0..size {
            if let Some(fa) = &self.buffer[pos as usize].attribute.field_attribute {
                if !fa.protected {
                    // Check if cursor is already in this field's data area
                    let data_start = (pos + 1) % size;
                    if data_start == start {
                        // We're at the start of the current field, keep going back
                    } else {
                        self.cursor_position = data_start;
                        return;
                    }
                }
            }
            if pos == 0 {
                pos = size - 1;
            } else {
                pos -= 1;
            }
        }
    }

    /// Generate a Read Modified response for the current screen state.
    ///
    /// The Read Modified response is sent from the terminal to the host and
    /// contains:
    /// 1. AID byte
    /// 2. Cursor address (2 bytes)
    /// 3. For each modified unprotected field: SBA + field data
    ///
    /// PA keys (PA1-PA3) and Clear send only the AID + cursor address
    /// (short read modified).
    pub fn read_modified_fields(&self, aid: Aid) -> Vec<u8> {
        let mut response = Vec::new();

        // AID byte
        response.push(aid.to_byte());

        // Cursor address (2 bytes, 12-bit encoding)
        let (cb1, cb2) = crate::datastream::encode_buffer_address(self.cursor_position);
        response.push(cb1);
        response.push(cb2);

        // PA keys and Clear only send AID + cursor (short read modified)
        match aid {
            Aid::Pa(_) | Aid::Clear => return response,
            _ => {}
        }

        // For each modified field, send SBA + field data
        let size = self.size() as usize;
        let fields = self.get_fields();

        for field in &fields {
            if field.attribute.protected || !field.modified {
                continue;
            }

            // SBA order pointing to the first data position of this field
            let data_start = ((field.start as usize + 1) % size) as u16;
            let (b1, b2) = crate::datastream::encode_buffer_address(data_start);
            response.push(0x11); // SBA order
            response.push(b1);
            response.push(b2);

            // Field data (EBCDIC bytes)
            let mut pos = data_start;
            loop {
                let cell = &self.buffer[pos as usize];
                // Stop if we hit another field attribute
                if pos != data_start && cell.attribute.field_attribute.is_some() {
                    break;
                }
                // Convert character back to EBCDIC
                let ebcdic = ebcdic::unicode_to_ebcdic(cell.character, self.code_page)
                    .unwrap_or(EBCDIC_SPACE);
                response.push(ebcdic);
                pos = ((pos as usize + 1) % size) as u16;
                if pos == data_start {
                    break; // wrapped around
                }
            }
        }

        response
    }

    /// Set a field's MDT (Modified Data Tag) flag.
    ///
    /// Called when the user types into a field.
    pub fn set_field_modified(&mut self, field_attr_pos: u16) {
        if let Some(ref mut fa) = self.buffer[field_attr_pos as usize]
            .attribute
            .field_attribute
        {
            fa.modified = true;
        }
    }

    /// Get the text content of a row as a Unicode string.
    ///
    /// Field attribute positions are rendered as spaces.
    pub fn get_row_text(&self, row: u16) -> String {
        if row >= self.rows {
            return String::new();
        }
        let start = (row as usize) * (self.cols as usize);
        let end = start + self.cols as usize;
        self.buffer[start..end]
            .iter()
            .map(|cell| {
                if cell.attribute.field_attribute.is_some() {
                    ' '
                } else {
                    cell.character
                }
            })
            .collect()
    }

    /// Get the full screen content as a multi-line string.
    pub fn get_screen_text(&self) -> String {
        let mut lines = Vec::with_capacity(self.rows as usize);
        for row in 0..self.rows {
            lines.push(self.get_row_text(row));
        }
        lines.join("\n")
    }

    /// Input a character at the current cursor position.
    ///
    /// Only works if the cursor is in an unprotected field. Returns true
    /// if the character was accepted, false if the field is protected.
    pub fn input_char(&mut self, ch: char) -> bool {
        if self.is_protected(self.cursor_position) {
            return false;
        }

        // Find the governing field attribute and set its MDT
        let size = self.size();
        let mut check = self.cursor_position;
        for _ in 0..size {
            if self.buffer[check as usize]
                .attribute
                .field_attribute
                .is_some()
            {
                self.set_field_modified(check);
                break;
            }
            if check == 0 {
                check = size - 1;
            } else {
                check -= 1;
            }
        }

        let pos = self.cursor_position as usize;
        self.buffer[pos].character = ch;
        self.advance();
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::datastream::{
        encode_buffer_address, parse_data_stream, DataStreamItem, Order, Wcc, WriteCommand,
    };

    fn make_stream(command: WriteCommand, wcc: Wcc, orders: Vec<DataStreamItem>) -> DataStream {
        DataStream {
            command,
            wcc,
            orders,
        }
    }

    fn default_wcc() -> Wcc {
        Wcc {
            reset_mdt: false,
            restore_keyboard: false,
            alarm: false,
        }
    }

    fn reset_wcc() -> Wcc {
        Wcc {
            reset_mdt: true,
            restore_keyboard: true,
            alarm: false,
        }
    }

    // -- Basic construction tests --

    #[test]
    fn test_new_screen_buffer() {
        let screen = ScreenBuffer::new(24, 80);
        assert_eq!(screen.rows(), 24);
        assert_eq!(screen.cols(), 80);
        assert_eq!(screen.size(), 1920);
        assert_eq!(screen.cursor_pos(), 0);
    }

    #[test]
    fn test_initial_cells_are_spaces() {
        let screen = ScreenBuffer::new(24, 80);
        for row in 0..24 {
            for col in 0..80 {
                let cell = screen.get_cell(row, col).unwrap();
                assert_eq!(cell.character, ' ');
                assert_eq!(cell.attribute.field_attribute, None);
            }
        }
    }

    #[test]
    fn test_get_cell_out_of_range() {
        let screen = ScreenBuffer::new(24, 80);
        assert!(screen.get_cell(24, 0).is_none());
        assert!(screen.get_cell(0, 80).is_none());
    }

    #[test]
    fn test_clear() {
        let mut screen = ScreenBuffer::new(24, 80);
        // Write something
        screen.buffer[0].character = 'X';
        screen.cursor_position = 100;
        screen.clear();
        assert_eq!(screen.buffer[0].character, ' ');
        assert_eq!(screen.cursor_pos(), 0);
    }

    // -- Erase/Write tests --

    #[test]
    fn test_erase_write_clears_screen() {
        let mut screen = ScreenBuffer::new(24, 80);
        screen.buffer[0].character = 'X';
        screen.buffer[100].character = 'Y';

        let stream = make_stream(WriteCommand::EraseWrite, reset_wcc(), vec![]);
        screen.apply_data_stream(&stream);

        assert_eq!(screen.buffer[0].character, ' ');
        assert_eq!(screen.buffer[100].character, ' ');
    }

    #[test]
    fn test_write_preserves_existing_content() {
        let mut screen = ScreenBuffer::new(24, 80);
        screen.buffer[50].character = 'X';

        let stream = make_stream(WriteCommand::Write, default_wcc(), vec![]);
        screen.apply_data_stream(&stream);

        assert_eq!(screen.buffer[50].character, 'X');
    }

    // -- Character data tests --

    #[test]
    fn test_write_characters() {
        let mut screen = ScreenBuffer::new(24, 80);

        // Write "HELLO" starting at position 0 (CP 037)
        let stream = make_stream(
            WriteCommand::EraseWrite,
            reset_wcc(),
            vec![
                DataStreamItem::Character(0xC8), // H
                DataStreamItem::Character(0xC5), // E
                DataStreamItem::Character(0xD3), // L
                DataStreamItem::Character(0xD3), // L
                DataStreamItem::Character(0xD6), // O
            ],
        );
        screen.apply_data_stream(&stream);

        assert_eq!(screen.buffer[0].character, 'H');
        assert_eq!(screen.buffer[1].character, 'E');
        assert_eq!(screen.buffer[2].character, 'L');
        assert_eq!(screen.buffer[3].character, 'L');
        assert_eq!(screen.buffer[4].character, 'O');
        assert_eq!(screen.cursor_pos(), 5);
    }

    // -- SBA order tests --

    #[test]
    fn test_sba_positions_cursor() {
        let mut screen = ScreenBuffer::new(24, 80);

        let stream = make_stream(
            WriteCommand::EraseWrite,
            reset_wcc(),
            vec![
                DataStreamItem::Order(Order::Sba(160)),
                DataStreamItem::Character(0xC8), // H
            ],
        );
        screen.apply_data_stream(&stream);

        assert_eq!(screen.buffer[160].character, 'H');
        assert_eq!(screen.cursor_pos(), 161);
    }

    // -- SF order tests --

    #[test]
    fn test_sf_creates_field() {
        let mut screen = ScreenBuffer::new(24, 80);

        let fa = FieldAttribute {
            protected: true,
            numeric: false,
            intensity: Intensity::Normal,
            modified: false,
        };

        let stream = make_stream(
            WriteCommand::EraseWrite,
            reset_wcc(),
            vec![
                DataStreamItem::Order(Order::Sba(10)),
                DataStreamItem::Order(Order::Sf(fa)),
                DataStreamItem::Character(0xD3), // L
                DataStreamItem::Character(0xD6), // O
                DataStreamItem::Character(0xC7), // G
                DataStreamItem::Character(0xD6), // O
                DataStreamItem::Character(0xD5), // N
            ],
        );
        screen.apply_data_stream(&stream);

        // Position 10 should have the field attribute
        assert!(screen.buffer[10].attribute.field_attribute.is_some());
        assert!(
            screen.buffer[10]
                .attribute
                .field_attribute
                .unwrap()
                .protected
        );

        // Characters should be at positions 11-15
        assert_eq!(screen.buffer[11].character, 'L');
        assert_eq!(screen.buffer[15].character, 'N');
    }

    // -- Field discovery tests --

    #[test]
    fn test_get_fields_single_field() {
        let mut screen = ScreenBuffer::new(24, 80);

        let fa = FieldAttribute {
            protected: false,
            numeric: false,
            intensity: Intensity::Normal,
            modified: false,
        };

        let stream = make_stream(
            WriteCommand::EraseWrite,
            reset_wcc(),
            vec![DataStreamItem::Order(Order::Sf(fa))],
        );
        screen.apply_data_stream(&stream);

        let fields = screen.get_fields();
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].start, 0);
        assert!(!fields[0].attribute.protected);
    }

    #[test]
    fn test_get_fields_two_fields() {
        let mut screen = ScreenBuffer::new(24, 80);

        let prot = FieldAttribute {
            protected: true,
            numeric: false,
            intensity: Intensity::Normal,
            modified: false,
        };
        let unprot = FieldAttribute {
            protected: false,
            numeric: false,
            intensity: Intensity::Normal,
            modified: false,
        };

        let stream = make_stream(
            WriteCommand::EraseWrite,
            reset_wcc(),
            vec![
                DataStreamItem::Order(Order::Sba(0)),
                DataStreamItem::Order(Order::Sf(prot)),
                DataStreamItem::Character(0xD3), // L
                DataStreamItem::Character(0xD6), // O
                DataStreamItem::Character(0xC7), // G
                DataStreamItem::Character(0xD6), // O
                DataStreamItem::Character(0xD5), // N
                DataStreamItem::Order(Order::Sf(unprot)),
            ],
        );
        screen.apply_data_stream(&stream);

        let fields = screen.get_fields();
        assert_eq!(fields.len(), 2);
        assert!(fields[0].attribute.protected);
        assert!(!fields[1].attribute.protected);
    }

    // -- IC (Insert Cursor) test --

    #[test]
    fn test_ic_sets_cursor() {
        let mut screen = ScreenBuffer::new(24, 80);

        let stream = make_stream(
            WriteCommand::EraseWrite,
            reset_wcc(),
            vec![
                DataStreamItem::Order(Order::Sba(100)),
                DataStreamItem::Order(Order::Ic),
            ],
        );
        screen.apply_data_stream(&stream);

        // IC doesn't move the cursor; it marks the current position as the
        // visible cursor. Since SBA set it to 100, cursor stays at 100.
        assert_eq!(screen.cursor_pos(), 100);
    }

    // -- RA (Repeat to Address) test --

    #[test]
    fn test_ra_fills_range() {
        let mut screen = ScreenBuffer::new(24, 80);

        // Fill positions 10-19 with EBCDIC space (0x40)
        let stream = make_stream(
            WriteCommand::EraseWrite,
            reset_wcc(),
            vec![
                DataStreamItem::Order(Order::Sba(10)),
                DataStreamItem::Order(Order::Ra(20, 0x60)), // EBCDIC '-' = 0x60
            ],
        );
        screen.apply_data_stream(&stream);

        for i in 10..20 {
            assert_eq!(
                screen.buffer[i].character, '-',
                "Position {} should be '-'",
                i
            );
        }
        assert_eq!(screen.cursor_pos(), 20);
    }

    // -- EUA (Erase Unprotected to Address) test --

    #[test]
    fn test_eua_clears_unprotected() {
        let mut screen = ScreenBuffer::new(24, 80);

        // Set up a screen with content
        let unprot = FieldAttribute {
            protected: false,
            numeric: false,
            intensity: Intensity::Normal,
            modified: false,
        };

        let stream = make_stream(
            WriteCommand::EraseWrite,
            reset_wcc(),
            vec![
                DataStreamItem::Order(Order::Sba(0)),
                DataStreamItem::Order(Order::Sf(unprot)),
                DataStreamItem::Character(0xC8), // H
                DataStreamItem::Character(0xC5), // E
                DataStreamItem::Character(0xD3), // L
                DataStreamItem::Character(0xD3), // L
                DataStreamItem::Character(0xD6), // O
            ],
        );
        screen.apply_data_stream(&stream);

        // Now EUA from position 1 to position 6
        let eua_stream = make_stream(
            WriteCommand::Write,
            default_wcc(),
            vec![
                DataStreamItem::Order(Order::Sba(1)),
                DataStreamItem::Order(Order::Eua(6)),
            ],
        );
        screen.apply_data_stream(&eua_stream);

        // Positions 1-5 should be cleared (they're in the unprotected field)
        for i in 1..6 {
            assert_eq!(
                screen.buffer[i].character, ' ',
                "Position {} should be cleared",
                i
            );
        }
    }

    // -- Tab navigation tests --

    #[test]
    fn test_tab_forward() {
        let mut screen = ScreenBuffer::new(24, 80);

        let prot = FieldAttribute {
            protected: true,
            numeric: false,
            intensity: Intensity::Normal,
            modified: false,
        };
        let unprot = FieldAttribute {
            protected: false,
            numeric: false,
            intensity: Intensity::Normal,
            modified: false,
        };

        let stream = make_stream(
            WriteCommand::EraseWrite,
            reset_wcc(),
            vec![
                DataStreamItem::Order(Order::Sba(0)),
                DataStreamItem::Order(Order::Sf(prot)),
                DataStreamItem::Character(0xD3), // label
                DataStreamItem::Character(0xD6),
                DataStreamItem::Character(0xC7),
                DataStreamItem::Character(0xD6),
                DataStreamItem::Character(0xD5),
                // Position 6: unprotected field
                DataStreamItem::Order(Order::Sf(unprot)),
            ],
        );
        screen.apply_data_stream(&stream);

        // Cursor is at 7 (after the unprot SF)
        // Set cursor to 0 and tab forward
        screen.cursor_position = 0;
        screen.tab_forward();

        // Should land at position 7 (first data position of unprotected field at pos 6)
        assert_eq!(screen.cursor_pos(), 7);
    }

    #[test]
    fn test_tab_backward() {
        let mut screen = ScreenBuffer::new(24, 80);

        let prot = FieldAttribute {
            protected: true,
            numeric: false,
            intensity: Intensity::Normal,
            modified: false,
        };
        let unprot = FieldAttribute {
            protected: false,
            numeric: false,
            intensity: Intensity::Normal,
            modified: false,
        };

        let stream = make_stream(
            WriteCommand::EraseWrite,
            reset_wcc(),
            vec![
                // First field: unprotected at position 0
                DataStreamItem::Order(Order::Sba(0)),
                DataStreamItem::Order(Order::Sf(unprot)),
                DataStreamItem::Character(0x40), // spaces
                DataStreamItem::Character(0x40),
                DataStreamItem::Character(0x40),
                DataStreamItem::Character(0x40),
                DataStreamItem::Character(0x40),
                // Position 6: protected field
                DataStreamItem::Order(Order::Sf(prot)),
                DataStreamItem::Character(0xD3),
                DataStreamItem::Character(0xD6),
                // Position 9: second unprotected field
                DataStreamItem::Order(Order::Sf(unprot)),
            ],
        );
        screen.apply_data_stream(&stream);

        // Set cursor to 15 (well past both fields), tab backward
        screen.cursor_position = 15;
        screen.tab_backward();

        // Should find the unprotected field at position 9, cursor at 10
        assert_eq!(screen.cursor_pos(), 10);
    }

    // -- WCC reset MDT test --

    #[test]
    fn test_wcc_reset_mdt() {
        let mut screen = ScreenBuffer::new(24, 80);

        let modified_fa = FieldAttribute {
            protected: false,
            numeric: false,
            intensity: Intensity::Normal,
            modified: true,
        };

        let stream = make_stream(
            WriteCommand::EraseWrite,
            default_wcc(),
            vec![DataStreamItem::Order(Order::Sf(modified_fa))],
        );
        screen.apply_data_stream(&stream);

        assert!(screen.buffer[0].attribute.field_attribute.unwrap().modified);

        // Now apply a stream with reset_mdt
        let reset_stream = make_stream(
            WriteCommand::Write,
            Wcc {
                reset_mdt: true,
                restore_keyboard: false,
                alarm: false,
            },
            vec![],
        );
        screen.apply_data_stream(&reset_stream);

        assert!(!screen.buffer[0].attribute.field_attribute.unwrap().modified);
    }

    // -- Read Modified test --

    #[test]
    fn test_read_modified_fields_enter() {
        let mut screen = ScreenBuffer::new(24, 80);

        let prot = FieldAttribute {
            protected: true,
            numeric: false,
            intensity: Intensity::Normal,
            modified: false,
        };
        let modified_unprot = FieldAttribute {
            protected: false,
            numeric: false,
            intensity: Intensity::Normal,
            modified: true,
        };

        let stream = make_stream(
            WriteCommand::EraseWrite,
            default_wcc(),
            vec![
                DataStreamItem::Order(Order::Sba(0)),
                DataStreamItem::Order(Order::Sf(prot)),
                DataStreamItem::Character(0xD3), // L
                DataStreamItem::Character(0xD6), // O
                DataStreamItem::Character(0xC7), // G
                DataStreamItem::Order(Order::Sf(modified_unprot)),
                DataStreamItem::Character(0xE4), // U
                DataStreamItem::Character(0xE2), // S
                DataStreamItem::Character(0xC5), // E
                DataStreamItem::Character(0xD9), // R
            ],
        );
        screen.apply_data_stream(&stream);

        screen.cursor_position = 5;
        let response = screen.read_modified_fields(Aid::Enter);

        // Response should be: AID(Enter) + cursor_addr(2 bytes) + SBA + field data
        assert!(!response.is_empty());
        assert_eq!(response[0], Aid::ENTER);
        // The remaining bytes contain cursor address + SBA orders + field data
        assert!(response.len() > 3);
    }

    #[test]
    fn test_read_modified_pa_key_short_response() {
        let mut screen = ScreenBuffer::new(24, 80);
        screen.cursor_position = 0;

        let response = screen.read_modified_fields(Aid::Pa(1));
        // PA key: only AID + cursor address
        assert_eq!(response.len(), 3);
        assert_eq!(response[0], Aid::PA1);
    }

    // -- Row text extraction tests --

    #[test]
    fn test_get_row_text() {
        let mut screen = ScreenBuffer::new(24, 80);

        let stream = make_stream(
            WriteCommand::EraseWrite,
            reset_wcc(),
            vec![
                DataStreamItem::Order(Order::Sba(0)),
                DataStreamItem::Character(0xC8), // H
                DataStreamItem::Character(0xC5), // E
                DataStreamItem::Character(0xD3), // L
                DataStreamItem::Character(0xD3), // L
                DataStreamItem::Character(0xD6), // O
            ],
        );
        screen.apply_data_stream(&stream);

        let text = screen.get_row_text(0);
        assert!(text.starts_with("HELLO"));
        assert_eq!(text.len(), 80);
    }

    #[test]
    fn test_get_row_text_field_attr_as_space() {
        let mut screen = ScreenBuffer::new(24, 80);

        let fa = FieldAttribute {
            protected: true,
            numeric: false,
            intensity: Intensity::Normal,
            modified: false,
        };

        let stream = make_stream(
            WriteCommand::EraseWrite,
            reset_wcc(),
            vec![
                DataStreamItem::Order(Order::Sba(0)),
                DataStreamItem::Order(Order::Sf(fa)),
                DataStreamItem::Character(0xC8), // H
                DataStreamItem::Character(0xC9), // I
            ],
        );
        screen.apply_data_stream(&stream);

        let text = screen.get_row_text(0);
        // Position 0 is the field attribute, displayed as space
        assert_eq!(&text[0..3], " HI");
    }

    // -- Input test --

    #[test]
    fn test_input_char_unprotected() {
        let mut screen = ScreenBuffer::new(24, 80);

        let unprot = FieldAttribute {
            protected: false,
            numeric: false,
            intensity: Intensity::Normal,
            modified: false,
        };

        let stream = make_stream(
            WriteCommand::EraseWrite,
            default_wcc(),
            vec![
                DataStreamItem::Order(Order::Sba(0)),
                DataStreamItem::Order(Order::Sf(unprot)),
            ],
        );
        screen.apply_data_stream(&stream);

        screen.cursor_position = 1; // First data position
        assert!(screen.input_char('A'));
        assert_eq!(screen.buffer[1].character, 'A');
        assert_eq!(screen.cursor_pos(), 2);

        // Verify MDT was set
        assert!(screen.buffer[0].attribute.field_attribute.unwrap().modified);
    }

    #[test]
    fn test_input_char_protected_rejected() {
        let mut screen = ScreenBuffer::new(24, 80);

        let prot = FieldAttribute {
            protected: true,
            numeric: false,
            intensity: Intensity::Normal,
            modified: false,
        };

        let stream = make_stream(
            WriteCommand::EraseWrite,
            default_wcc(),
            vec![
                DataStreamItem::Order(Order::Sba(0)),
                DataStreamItem::Order(Order::Sf(prot)),
            ],
        );
        screen.apply_data_stream(&stream);

        screen.cursor_position = 1;
        assert!(!screen.input_char('A'));
        assert_eq!(screen.buffer[1].character, ' '); // unchanged
    }

    // -- Cursor position tests --

    #[test]
    fn test_cursor_row_col() {
        let mut screen = ScreenBuffer::new(24, 80);
        screen.cursor_position = 163; // row 2, col 3
        assert_eq!(screen.cursor_row_col(), (2, 3));
    }

    #[test]
    fn test_cursor_wraps_at_end() {
        let mut screen = ScreenBuffer::new(24, 80);
        screen.cursor_position = 1919; // last position
        screen.advance();
        assert_eq!(screen.cursor_pos(), 0); // wrapped to beginning
    }

    // -- Full data stream parse + apply integration test --

    #[test]
    fn test_parse_and_apply_login_screen() {
        let (sba0_b1, sba0_b2) = encode_buffer_address(0);
        let (sba80_b1, sba80_b2) = encode_buffer_address(80);

        let raw_data = [
            0x05, // Erase/Write
            0x60, // WCC: reset_mdt + restore_keyboard
            0x11, sba0_b1, sba0_b2, // SBA(0)
            0x1D, 0x20, // SF(protected)
            0xD3, 0xD6, 0xC7, 0xD6, 0xD5, // "LOGON"
            0x11, sba80_b1, sba80_b2, // SBA(80)
            0x1D, 0x00, // SF(unprotected)
            0x13, // IC
        ];

        let ds = parse_data_stream(&raw_data).unwrap();
        let mut screen = ScreenBuffer::new(24, 80);
        screen.apply_data_stream(&ds);

        // Row 0 should contain: [FA]LOGON followed by spaces
        let row0 = screen.get_row_text(0);
        assert!(row0.starts_with(" LOGON"), "Row 0: '{}'", &row0[..10]);

        // Position 0 should have a protected field attribute
        assert!(screen.buffer[0].attribute.field_attribute.is_some());
        assert!(
            screen.buffer[0]
                .attribute
                .field_attribute
                .unwrap()
                .protected
        );

        // Position 80 should have an unprotected field attribute
        assert!(screen.buffer[80].attribute.field_attribute.is_some());
        assert!(
            !screen.buffer[80]
                .attribute
                .field_attribute
                .unwrap()
                .protected
        );

        // Cursor should be at 81 (IC was at position 81 after the SF at 80 advanced to 81)
        assert_eq!(screen.cursor_pos(), 81);
    }

    // -- Alternate screen size test --

    #[test]
    fn test_alternate_screen_size() {
        let screen = ScreenBuffer::new(43, 80);
        assert_eq!(screen.size(), 3440);

        let screen132 = ScreenBuffer::new(27, 132);
        assert_eq!(screen132.size(), 3564);
    }

    // -- SFE order test on screen --

    #[test]
    fn test_sfe_with_color() {
        let mut screen = ScreenBuffer::new(24, 80);

        let stream = make_stream(
            WriteCommand::EraseWrite,
            reset_wcc(),
            vec![
                DataStreamItem::Order(Order::Sba(0)),
                DataStreamItem::Order(Order::Sfe(vec![
                    ExtendedAttribute::FieldAttribute(FieldAttribute {
                        protected: true,
                        numeric: false,
                        intensity: Intensity::Normal,
                        modified: false,
                    }),
                    ExtendedAttribute::ForegroundColor(Color3270::Red),
                ])),
                DataStreamItem::Character(0xC8), // H
                DataStreamItem::Character(0xC9), // I
            ],
        );
        screen.apply_data_stream(&stream);

        // Position 0 should have field attribute and red foreground
        assert!(screen.buffer[0].attribute.field_attribute.is_some());
        assert_eq!(screen.buffer[0].attribute.foreground, Color3270::Red);
    }

    // -- SA (Set Attribute) test on screen --

    #[test]
    fn test_sa_changes_subsequent_characters() {
        let mut screen = ScreenBuffer::new(24, 80);

        let stream = make_stream(
            WriteCommand::EraseWrite,
            reset_wcc(),
            vec![
                DataStreamItem::Order(Order::Sba(0)),
                DataStreamItem::Character(0xC8), // H - default color
                DataStreamItem::Order(Order::Sa(ExtendedAttribute::ForegroundColor(
                    Color3270::Green,
                ))),
                DataStreamItem::Character(0xC9), // I - green
                DataStreamItem::Character(0x5A), // ! - green
            ],
        );
        screen.apply_data_stream(&stream);

        assert_eq!(screen.buffer[0].attribute.foreground, Color3270::Default);
        assert_eq!(screen.buffer[1].attribute.foreground, Color3270::Green);
        assert_eq!(screen.buffer[2].attribute.foreground, Color3270::Green);
    }

    // -- get_field_at test --

    #[test]
    fn test_get_field_at() {
        let mut screen = ScreenBuffer::new(24, 80);

        let prot = FieldAttribute {
            protected: true,
            numeric: false,
            intensity: Intensity::Normal,
            modified: false,
        };
        let unprot = FieldAttribute {
            protected: false,
            numeric: false,
            intensity: Intensity::Normal,
            modified: false,
        };

        let stream = make_stream(
            WriteCommand::EraseWrite,
            reset_wcc(),
            vec![
                DataStreamItem::Order(Order::Sba(0)),
                DataStreamItem::Order(Order::Sf(prot)),
                DataStreamItem::Character(0xC8),
                DataStreamItem::Character(0xC5),
                DataStreamItem::Character(0xD3),
                DataStreamItem::Character(0xD3),
                DataStreamItem::Character(0xD6),
                DataStreamItem::Order(Order::Sf(unprot)),
            ],
        );
        screen.apply_data_stream(&stream);

        let field_at_0 = screen.get_field_at(0).unwrap();
        assert!(field_at_0.attribute.protected);

        let field_at_6 = screen.get_field_at(6).unwrap();
        assert!(!field_at_6.attribute.protected);
    }

    // -- Code page test --

    #[test]
    fn test_with_code_page_cp500() {
        let mut screen = ScreenBuffer::with_code_page(24, 80, CodePage::Cp500);

        let stream = make_stream(
            WriteCommand::EraseWrite,
            reset_wcc(),
            vec![
                DataStreamItem::Character(0xC8), // H (same across code pages)
                DataStreamItem::Character(0xC9), // I
            ],
        );
        screen.apply_data_stream(&stream);

        assert_eq!(screen.buffer[0].character, 'H');
        assert_eq!(screen.buffer[1].character, 'I');
    }

    // -- Screen text extraction test --

    #[test]
    fn test_get_screen_text() {
        let mut screen = ScreenBuffer::new(3, 10); // small for testing

        let stream = make_stream(
            WriteCommand::EraseWrite,
            reset_wcc(),
            vec![
                DataStreamItem::Order(Order::Sba(0)),
                DataStreamItem::Character(0xC8), // H
                DataStreamItem::Character(0xC5), // E
                DataStreamItem::Character(0xD3), // L
                DataStreamItem::Character(0xD3), // L
                DataStreamItem::Character(0xD6), // O
            ],
        );
        screen.apply_data_stream(&stream);

        let text = screen.get_screen_text();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3);
        assert!(lines[0].starts_with("HELLO"));
    }
}
