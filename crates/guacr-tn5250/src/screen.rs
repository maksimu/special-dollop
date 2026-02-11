//! 5250 Screen Buffer
//!
//! Maintains a 2D grid of cells representing the AS/400 display, along with
//! field definitions. The screen buffer is updated by applying parsed 5250
//! records from the data stream parser.
//!
//! The standard screen size is 24 rows by 80 columns, though 27x132 is also
//! supported by some implementations.

use crate::datastream::{
    DataStreamItem5250, FieldControlWord, FieldShift, OpCode, Order, Record5250,
};
use crate::ebcdic;
use crate::ebcdic::CodePage;
use log::{debug, trace};

/// Default EBCDIC code page for 5250 (AS/400 uses CP 037).
const DEFAULT_CODE_PAGE: CodePage = CodePage::Cp037;

// ---------------------------------------------------------------------------
// Color
// ---------------------------------------------------------------------------

/// Display colors supported by the 5250 protocol.
///
/// The IBM 5250 terminal supports a fixed palette. Most AS/400 screens use
/// green-on-black, but applications can select other colors via extended
/// attributes.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Color5250 {
    Black,
    #[default]
    Green,
    White,
    Red,
    Blue,
    Turquoise,
    Yellow,
    Pink,
}

// ---------------------------------------------------------------------------
// Cell
// ---------------------------------------------------------------------------

/// A single character cell on the 5250 screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cell5250 {
    /// The Unicode character displayed in this cell.
    pub character: char,
    /// Foreground (text) color.
    pub foreground: Color5250,
    /// Background color.
    pub background: Color5250,
    /// Whether the character is underlined.
    pub underline: bool,
    /// Whether this cell is the start of a field (attribute byte position).
    pub field_start: bool,
}

impl Default for Cell5250 {
    fn default() -> Self {
        Cell5250 {
            character: ' ',
            foreground: Color5250::Green,
            background: Color5250::Black,
            underline: false,
            field_start: false,
        }
    }
}

// ---------------------------------------------------------------------------
// Field
// ---------------------------------------------------------------------------

/// A field definition on the 5250 screen.
///
/// Fields are input areas where the operator can type data. Each field has a
/// position, length, and control word defining its behaviour.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field5250 {
    /// 0-based row of the field attribute byte.
    pub row: u16,
    /// 0-based column of the field attribute byte.
    pub col: u16,
    /// Length of the field in characters (not counting the attribute byte).
    pub length: u16,
    /// Field Control Word defining behaviour.
    pub control: FieldControlWord,
    /// Modified Data Tag -- true if the operator has changed the field.
    pub modified: bool,
}

// ---------------------------------------------------------------------------
// Screen buffer
// ---------------------------------------------------------------------------

/// The main 5250 screen buffer.
///
/// Holds the cell grid, field definitions, and cursor position. Apply parsed
/// [`Record5250`] values via [`apply_record`](ScreenBuffer5250::apply_record)
/// to update the screen state.
pub struct ScreenBuffer5250 {
    rows: u16,
    cols: u16,
    buffer: Vec<Cell5250>,
    cursor_row: u16,
    cursor_col: u16,
    fields: Vec<Field5250>,
}

impl ScreenBuffer5250 {
    /// Create a new screen buffer with the given dimensions.
    ///
    /// Standard 5250 screens are 24x80. Some applications use 27x132.
    pub fn new(rows: u16, cols: u16) -> Self {
        let total = (rows as usize) * (cols as usize);
        ScreenBuffer5250 {
            rows,
            cols,
            buffer: vec![Cell5250::default(); total],
            cursor_row: 0,
            cursor_col: 0,
            fields: Vec::new(),
        }
    }

    /// Number of rows in the screen.
    pub fn rows(&self) -> u16 {
        self.rows
    }

    /// Number of columns in the screen.
    pub fn cols(&self) -> u16 {
        self.cols
    }

    /// Return the current cursor position as (row, col), both 0-based.
    pub fn cursor_pos(&self) -> (u16, u16) {
        (self.cursor_row, self.cursor_col)
    }

    /// Return a reference to the cell at (row, col), both 0-based.
    ///
    /// Returns `None` if the coordinates are out of bounds.
    pub fn get_cell(&self, row: u16, col: u16) -> Option<&Cell5250> {
        if row < self.rows && col < self.cols {
            Some(&self.buffer[self.offset(row, col)])
        } else {
            None
        }
    }

    /// Return the list of currently defined fields.
    pub fn get_fields(&self) -> &[Field5250] {
        &self.fields
    }

    /// Apply a parsed 5250 record to update the screen state.
    pub fn apply_record(&mut self, record: &Record5250) {
        match record.opcode {
            OpCode::ClearUnit => {
                self.clear();
            }
            OpCode::ClearFormatTable => {
                self.clear_fields();
            }
            OpCode::WriteToDisplay | OpCode::WriteToDisplayAlt => {
                self.apply_wtd(&record.orders);
            }
        }
    }

    /// Read the contents of the field at the given index as a Unicode string.
    ///
    /// Returns `None` if the index is out of range.
    pub fn read_field(&self, field_index: usize) -> Option<String> {
        let field = self.fields.get(field_index)?;
        let mut result = String::with_capacity(field.length as usize);
        let mut row = field.row;
        let mut col = field.col;
        // Skip the attribute byte position itself.
        self.advance_pos(&mut row, &mut col);
        for _ in 0..field.length {
            let cell = &self.buffer[self.offset(row, col)];
            result.push(cell.character);
            self.advance_pos(&mut row, &mut col);
        }
        Some(result)
    }

    /// Return all fields that have been modified (MDT set), along with their
    /// content encoded in EBCDIC. This is used to build the response record
    /// that the client sends back to the host.
    ///
    /// Each entry is `(row, col, ebcdic_data)` where row and col are 0-based
    /// positions of the field attribute byte.
    pub fn read_modified_fields(&self) -> Vec<(u16, u16, Vec<u8>)> {
        let mut result = Vec::new();
        for (i, field) in self.fields.iter().enumerate() {
            if field.modified {
                if let Some(text) = self.read_field(i) {
                    let ebcdic_data = ebcdic::encode_string(&text, DEFAULT_CODE_PAGE);
                    result.push((field.row, field.col, ebcdic_data));
                }
            }
        }
        result
    }

    /// Move the cursor to the next unprotected (input) field.
    ///
    /// If there are no unprotected fields, the cursor does not move.
    /// Returns `true` if the cursor moved.
    pub fn tab_forward(&mut self) -> bool {
        if self.fields.is_empty() {
            return false;
        }

        // Find the field that the cursor is currently in or after.
        let current_idx = self.field_index_at_cursor();

        // Search forward from the field after the current one.
        let start = current_idx.map(|i| i + 1).unwrap_or(0);
        let count = self.fields.len();

        for offset in 0..count {
            let idx = (start + offset) % count;
            let field = &self.fields[idx];
            if !field.control.bypass {
                // Move cursor to first data position of this field (one past
                // the attribute byte).
                let mut row = field.row;
                let mut col = field.col;
                self.advance_pos(&mut row, &mut col);
                self.cursor_row = row;
                self.cursor_col = col;
                return true;
            }
        }
        false
    }

    /// Move the cursor to the previous unprotected (input) field.
    ///
    /// Returns `true` if the cursor moved.
    pub fn tab_backward(&mut self) -> bool {
        if self.fields.is_empty() {
            return false;
        }

        let current_idx = self.field_index_at_cursor();
        let start = current_idx.unwrap_or(0);
        let count = self.fields.len();

        for offset in 1..=count {
            let idx = (start + count - offset) % count;
            let field = &self.fields[idx];
            if !field.control.bypass {
                let mut row = field.row;
                let mut col = field.col;
                self.advance_pos(&mut row, &mut col);
                self.cursor_row = row;
                self.cursor_col = col;
                return true;
            }
        }
        false
    }

    /// Write a Unicode character at the current cursor position as if the
    /// operator typed it. Advances the cursor. Sets the MDT on the enclosing
    /// field.
    ///
    /// Returns `false` if the cursor is in a protected field or there is no
    /// field at the cursor position.
    pub fn type_character(&mut self, ch: char) -> bool {
        if let Some(idx) = self.field_index_at_cursor() {
            let field = &self.fields[idx];
            if field.control.bypass {
                return false;
            }

            // Validate character against field shift type.
            if !self.is_valid_for_shift(ch, field.control.shift) {
                return false;
            }

            let offset = self.offset(self.cursor_row, self.cursor_col);
            self.buffer[offset].character = ch;
            self.fields[idx].modified = true;
            self.advance_cursor();
            true
        } else {
            false
        }
    }

    /// Extract the entire screen content as a string of rows separated by
    /// newlines. Trailing spaces on each row are preserved.
    pub fn screen_text(&self) -> String {
        let mut result = String::with_capacity((self.rows as usize) * (self.cols as usize + 1));
        for row in 0..self.rows {
            if row > 0 {
                result.push('\n');
            }
            for col in 0..self.cols {
                let cell = &self.buffer[self.offset(row, col)];
                result.push(cell.character);
            }
        }
        result
    }

    // -----------------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------------

    /// Linear offset into the buffer for the given (row, col).
    fn offset(&self, row: u16, col: u16) -> usize {
        (row as usize) * (self.cols as usize) + (col as usize)
    }

    /// Advance a (row, col) position by one cell, wrapping at end of screen.
    fn advance_pos(&self, row: &mut u16, col: &mut u16) {
        *col += 1;
        if *col >= self.cols {
            *col = 0;
            *row += 1;
            if *row >= self.rows {
                *row = 0;
            }
        }
    }

    /// Advance the cursor by one position.
    fn advance_cursor(&mut self) {
        let mut r = self.cursor_row;
        let mut c = self.cursor_col;
        self.advance_pos(&mut r, &mut c);
        self.cursor_row = r;
        self.cursor_col = c;
    }

    /// Clear the entire screen: reset all cells and remove all fields.
    fn clear(&mut self) {
        debug!("ClearUnit: resetting {} x {} screen", self.rows, self.cols);
        for cell in &mut self.buffer {
            *cell = Cell5250::default();
        }
        self.fields.clear();
        self.cursor_row = 0;
        self.cursor_col = 0;
    }

    /// Clear only the field definitions, leaving screen content intact.
    fn clear_fields(&mut self) {
        debug!("ClearFormatTable: removing {} fields", self.fields.len());
        // Reset field_start flags in cells.
        for field in &self.fields {
            let off = self.offset(field.row, field.col);
            if off < self.buffer.len() {
                self.buffer[off].field_start = false;
            }
        }
        self.fields.clear();
    }

    /// Apply a Write to Display payload.
    fn apply_wtd(&mut self, items: &[DataStreamItem5250]) {
        let mut write_row = self.cursor_row;
        let mut write_col = self.cursor_col;

        for item in items {
            match item {
                DataStreamItem5250::Order(order) => match order {
                    Order::Sba(row, col) => {
                        write_row = *row;
                        write_col = *col;
                        trace!("WTD: SBA -> ({}, {})", write_row, write_col);
                    }

                    Order::Sf(fcw) => {
                        // Mark the attribute byte position.
                        if write_row < self.rows && write_col < self.cols {
                            let off = self.offset(write_row, write_col);
                            self.buffer[off].field_start = true;
                            self.buffer[off].character = ' '; // attribute position is blank

                            // Determine field length: count cells until we hit
                            // another field start or end of screen. We calculate
                            // it later when the next field or end is known, but
                            // for now we add the field with length 0 and fix it
                            // up at the end.
                            self.fields.push(Field5250 {
                                row: write_row,
                                col: write_col,
                                length: 0, // will be computed in fixup
                                control: *fcw,
                                modified: fcw.modified,
                            });

                            trace!(
                                "WTD: SF at ({}, {}), bypass={}, shift={:?}",
                                write_row,
                                write_col,
                                fcw.bypass,
                                fcw.shift
                            );
                        }
                        self.advance_pos(&mut write_row, &mut write_col);
                    }

                    Order::Ra(target_row, target_col, fill_byte) => {
                        let fill_char = ebcdic::ebcdic_to_unicode(*fill_byte, DEFAULT_CODE_PAGE);
                        trace!(
                            "WTD: RA from ({},{}) to ({},{}) with '{}'",
                            write_row,
                            write_col,
                            target_row,
                            target_col,
                            fill_char
                        );
                        loop {
                            if write_row < self.rows && write_col < self.cols {
                                let off = self.offset(write_row, write_col);
                                self.buffer[off].character = fill_char;
                            }
                            if write_row == *target_row && write_col == *target_col {
                                self.advance_pos(&mut write_row, &mut write_col);
                                break;
                            }
                            self.advance_pos(&mut write_row, &mut write_col);
                            // Safety: prevent infinite loop if target is behind us
                            // (wraps around the entire screen once).
                        }
                    }

                    Order::Ea(target_row, target_col) => {
                        trace!(
                            "WTD: EA from ({},{}) to ({},{})",
                            write_row,
                            write_col,
                            target_row,
                            target_col
                        );
                        loop {
                            if write_row < self.rows && write_col < self.cols {
                                let off = self.offset(write_row, write_col);
                                self.buffer[off].character = ' ';
                            }
                            if write_row == *target_row && write_col == *target_col {
                                self.advance_pos(&mut write_row, &mut write_col);
                                break;
                            }
                            self.advance_pos(&mut write_row, &mut write_col);
                        }
                    }

                    Order::Td(bytes) => {
                        trace!("WTD: TD {} bytes", bytes.len());
                        for &b in bytes {
                            if write_row < self.rows && write_col < self.cols {
                                let off = self.offset(write_row, write_col);
                                self.buffer[off].character =
                                    ebcdic::ebcdic_to_unicode(b, DEFAULT_CODE_PAGE);
                            }
                            self.advance_pos(&mut write_row, &mut write_col);
                        }
                    }

                    Order::Ic => {
                        self.cursor_row = write_row;
                        self.cursor_col = write_col;
                        trace!("WTD: IC -> ({}, {})", write_row, write_col);
                    }

                    Order::Wea(attr) => {
                        // Apply extended attribute to the current position.
                        if write_row < self.rows && write_col < self.cols {
                            let off = self.offset(write_row, write_col);
                            self.apply_extended_attribute(off, attr.attr_type, attr.attr_value);
                        }
                        trace!(
                            "WTD: WEA type=0x{:02X} value=0x{:02X} at ({},{})",
                            attr.attr_type,
                            attr.attr_value,
                            write_row,
                            write_col
                        );
                    }

                    Order::Soh(_soh) => {
                        // SOH defines screen-level parameters. The data inside
                        // is application-specific and does not change the cell
                        // grid. We log it but otherwise treat it as a no-op for
                        // screen rendering.
                        trace!("WTD: SOH (screen-level format, {} bytes)", _soh.length);
                    }
                },

                DataStreamItem5250::Character(ebcdic_byte) => {
                    if write_row < self.rows && write_col < self.cols {
                        let off = self.offset(write_row, write_col);
                        self.buffer[off].character =
                            ebcdic::ebcdic_to_unicode(*ebcdic_byte, DEFAULT_CODE_PAGE);
                    }
                    self.advance_pos(&mut write_row, &mut write_col);
                }
            }
        }

        // Fix up field lengths: each field runs from its attribute byte + 1
        // until the next field's attribute byte (or end of screen).
        self.compute_field_lengths();
    }

    /// Compute field lengths based on the positions of consecutive fields.
    ///
    /// A field's data area starts one position after its attribute byte and
    /// extends until the position just before the next field's attribute byte
    /// (or until the end of the screen for the last field).
    fn compute_field_lengths(&mut self) {
        let total_cells = (self.rows as u32) * (self.cols as u32);
        let field_count = self.fields.len();
        if field_count == 0 {
            return;
        }

        for i in 0..field_count {
            let start_linear = self.linear_pos(self.fields[i].row, self.fields[i].col);
            // Data starts one position after the attribute byte.
            let data_start = (start_linear + 1) % total_cells;

            let end_linear = if i + 1 < field_count {
                self.linear_pos(self.fields[i + 1].row, self.fields[i + 1].col)
            } else if field_count > 1 {
                // Wrap: last field ends just before the first field.
                self.linear_pos(self.fields[0].row, self.fields[0].col)
            } else {
                // Only one field: it occupies the rest of the screen.
                start_linear
            };

            let length = if end_linear > data_start {
                end_linear - data_start
            } else if end_linear == data_start && field_count == 1 {
                // Single field wrapping the entire screen minus the attribute byte.
                total_cells - 1
            } else {
                // Wrap around.
                (total_cells - data_start) + end_linear
            };

            self.fields[i].length = length as u16;
        }
    }

    /// Convert (row, col) to a linear buffer position.
    fn linear_pos(&self, row: u16, col: u16) -> u32 {
        (row as u32) * (self.cols as u32) + (col as u32)
    }

    /// Find the field index that contains the current cursor position.
    ///
    /// Returns `None` if the cursor is not inside any field.
    fn field_index_at_cursor(&self) -> Option<usize> {
        let cursor_linear = self.linear_pos(self.cursor_row, self.cursor_col);
        let total_cells = (self.rows as u32) * (self.cols as u32);

        for (i, field) in self.fields.iter().enumerate() {
            let attr_linear = self.linear_pos(field.row, field.col);
            let data_start = (attr_linear + 1) % total_cells;
            let data_end = (data_start + field.length as u32) % total_cells;

            let inside = if data_start <= data_end {
                cursor_linear >= data_start && cursor_linear < data_end
            } else {
                // Field wraps around screen boundary.
                cursor_linear >= data_start || cursor_linear < data_end
            };

            if inside {
                return Some(i);
            }
        }
        None
    }

    /// Apply an extended attribute to a cell.
    fn apply_extended_attribute(&mut self, offset: usize, attr_type: u8, attr_value: u8) {
        if offset >= self.buffer.len() {
            return;
        }
        let cell = &mut self.buffer[offset];
        match attr_type {
            // 0x01 = character color / highlighting
            0x01 => {
                cell.foreground = color_from_5250_attribute(attr_value);
            }
            // 0x02 = background color
            0x02 => {
                cell.background = color_from_5250_attribute(attr_value);
            }
            // 0x03 = underline
            0x03 => {
                cell.underline = (attr_value & 0x01) != 0;
            }
            _ => {
                // Unknown attribute type -- ignore.
            }
        }
    }

    /// Check whether a character is valid for the given field shift type.
    fn is_valid_for_shift(&self, ch: char, shift: FieldShift) -> bool {
        match shift {
            FieldShift::Alpha => true,
            FieldShift::AlphaOnly => {
                ch.is_ascii_alphabetic() || matches!(ch, ' ' | '.' | ',' | '-' | '+')
            }
            FieldShift::Numeric => {
                ch.is_ascii_digit() || matches!(ch, ' ' | '.' | ',' | '-' | '+' | '$' | '*')
            }
            FieldShift::NumericOnly => ch.is_ascii_digit(),
            FieldShift::SignedNumeric => ch.is_ascii_digit() || matches!(ch, '+' | '-'),
            FieldShift::Katakana => true, // Accept anything for DBCS/Katakana
        }
    }
}

/// Map a 5250 color attribute value to a `Color5250`.
fn color_from_5250_attribute(value: u8) -> Color5250 {
    match value & 0x0F {
        0x00 => Color5250::Black,
        0x01 => Color5250::Blue,
        0x02 => Color5250::Green,
        0x03 => Color5250::Turquoise,
        0x04 => Color5250::Red,
        0x05 => Color5250::Pink,
        0x06 => Color5250::Yellow,
        0x07 | 0x0F => Color5250::White,
        _ => Color5250::Green, // default
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::datastream::{
        parse_5250_record, DataStreamItem5250, ExtendedAttribute, FieldControlWord, OpCode, Order,
        Record5250, SohData,
    };

    /// Helper: build a Record5250 directly from items (avoids byte-level construction).
    fn make_record(opcode: OpCode, orders: Vec<DataStreamItem5250>) -> Record5250 {
        Record5250 {
            record_type: 0x04,
            opcode,
            orders,
        }
    }

    // -- Basic construction -------------------------------------------------

    #[test]
    fn test_new_screen() {
        let screen = ScreenBuffer5250::new(24, 80);
        assert_eq!(screen.rows(), 24);
        assert_eq!(screen.cols(), 80);
        assert_eq!(screen.cursor_pos(), (0, 0));
        assert!(screen.get_fields().is_empty());
    }

    #[test]
    fn test_get_cell_default() {
        let screen = ScreenBuffer5250::new(24, 80);
        let cell = screen.get_cell(0, 0).unwrap();
        assert_eq!(cell.character, ' ');
        assert_eq!(cell.foreground, Color5250::Green);
        assert_eq!(cell.background, Color5250::Black);
        assert!(!cell.underline);
        assert!(!cell.field_start);
    }

    #[test]
    fn test_get_cell_out_of_bounds() {
        let screen = ScreenBuffer5250::new(24, 80);
        assert!(screen.get_cell(24, 0).is_none());
        assert!(screen.get_cell(0, 80).is_none());
    }

    // -- ClearUnit ----------------------------------------------------------

    #[test]
    fn test_clear_unit() {
        let mut screen = ScreenBuffer5250::new(24, 80);

        // Put something on screen first.
        let write = make_record(
            OpCode::WriteToDisplay,
            vec![
                DataStreamItem5250::Order(Order::Sba(0, 0)),
                DataStreamItem5250::Character(0xC8), // 'H'
            ],
        );
        screen.apply_record(&write);
        assert_eq!(screen.get_cell(0, 0).unwrap().character, 'H');

        // Clear.
        let clear = make_record(OpCode::ClearUnit, vec![]);
        screen.apply_record(&clear);
        assert_eq!(screen.get_cell(0, 0).unwrap().character, ' ');
        assert_eq!(screen.cursor_pos(), (0, 0));
    }

    // -- SBA + character write ----------------------------------------------

    #[test]
    fn test_sba_and_characters() {
        let mut screen = ScreenBuffer5250::new(24, 80);
        let record = make_record(
            OpCode::WriteToDisplay,
            vec![
                DataStreamItem5250::Order(Order::Sba(5, 10)),
                DataStreamItem5250::Character(0xC8), // 'H'
                DataStreamItem5250::Character(0xC9), // 'I'
            ],
        );
        screen.apply_record(&record);

        assert_eq!(screen.get_cell(5, 10).unwrap().character, 'H');
        assert_eq!(screen.get_cell(5, 11).unwrap().character, 'I');
        // Adjacent cell should still be blank.
        assert_eq!(screen.get_cell(5, 12).unwrap().character, ' ');
    }

    // -- Insert Cursor ------------------------------------------------------

    #[test]
    fn test_insert_cursor() {
        let mut screen = ScreenBuffer5250::new(24, 80);
        let record = make_record(
            OpCode::WriteToDisplay,
            vec![
                DataStreamItem5250::Order(Order::Sba(10, 20)),
                DataStreamItem5250::Order(Order::Ic),
            ],
        );
        screen.apply_record(&record);
        assert_eq!(screen.cursor_pos(), (10, 20));
    }

    // -- Repeat to Address --------------------------------------------------

    #[test]
    fn test_repeat_to_address() {
        let mut screen = ScreenBuffer5250::new(24, 80);
        // Fill from (0,0) to (0,4) with asterisks (0x5C = '*' in EBCDIC).
        let record = make_record(
            OpCode::WriteToDisplay,
            vec![
                DataStreamItem5250::Order(Order::Sba(0, 0)),
                DataStreamItem5250::Order(Order::Ra(0, 4, 0x5C)),
            ],
        );
        screen.apply_record(&record);

        for col in 0..=4 {
            assert_eq!(
                screen.get_cell(0, col).unwrap().character,
                '*',
                "col {} should be '*'",
                col
            );
        }
        // Col 5 should still be space.
        assert_eq!(screen.get_cell(0, 5).unwrap().character, ' ');
    }

    // -- Erase to Address ---------------------------------------------------

    #[test]
    fn test_erase_to_address() {
        let mut screen = ScreenBuffer5250::new(24, 80);

        // Write some characters first.
        let write = make_record(
            OpCode::WriteToDisplay,
            vec![
                DataStreamItem5250::Order(Order::Sba(0, 0)),
                DataStreamItem5250::Character(0xC1), // A
                DataStreamItem5250::Character(0xC2), // B
                DataStreamItem5250::Character(0xC3), // C
                DataStreamItem5250::Character(0xC4), // D
                DataStreamItem5250::Character(0xC5), // E
            ],
        );
        screen.apply_record(&write);
        assert_eq!(screen.get_cell(0, 0).unwrap().character, 'A');

        // Erase from (0,1) to (0,3).
        let erase = make_record(
            OpCode::WriteToDisplay,
            vec![
                DataStreamItem5250::Order(Order::Sba(0, 1)),
                DataStreamItem5250::Order(Order::Ea(0, 3)),
            ],
        );
        screen.apply_record(&erase);

        assert_eq!(screen.get_cell(0, 0).unwrap().character, 'A'); // untouched
        assert_eq!(screen.get_cell(0, 1).unwrap().character, ' '); // erased
        assert_eq!(screen.get_cell(0, 2).unwrap().character, ' '); // erased
        assert_eq!(screen.get_cell(0, 3).unwrap().character, ' '); // erased
        assert_eq!(screen.get_cell(0, 4).unwrap().character, 'E'); // untouched
    }

    // -- Start Field --------------------------------------------------------

    #[test]
    fn test_start_field() {
        let mut screen = ScreenBuffer5250::new(24, 80);
        let fcw = FieldControlWord::from_bytes(0x00, 0x00); // unprotected alpha
        let record = make_record(
            OpCode::WriteToDisplay,
            vec![
                DataStreamItem5250::Order(Order::Sba(2, 10)),
                DataStreamItem5250::Order(Order::Sf(fcw)),
            ],
        );
        screen.apply_record(&record);

        assert!(screen.get_cell(2, 10).unwrap().field_start);
        assert_eq!(screen.get_fields().len(), 1);
        let field = &screen.get_fields()[0];
        assert_eq!(field.row, 2);
        assert_eq!(field.col, 10);
        assert!(!field.control.bypass);
    }

    // -- Multiple fields and field length -----------------------------------

    #[test]
    fn test_multiple_fields_length_computation() {
        let mut screen = ScreenBuffer5250::new(24, 80);
        let fcw_input = FieldControlWord::from_bytes(0x00, 0x00);
        let fcw_protected = FieldControlWord::from_bytes(0x80, 0x00);

        // Field 1 at (0, 5), Field 2 at (0, 15)
        let record = make_record(
            OpCode::WriteToDisplay,
            vec![
                DataStreamItem5250::Order(Order::Sba(0, 5)),
                DataStreamItem5250::Order(Order::Sf(fcw_input)),
                DataStreamItem5250::Order(Order::Sba(0, 15)),
                DataStreamItem5250::Order(Order::Sf(fcw_protected)),
            ],
        );
        screen.apply_record(&record);

        let fields = screen.get_fields();
        assert_eq!(fields.len(), 2);
        // Field 1: data from (0,6) to (0,14) = 9 characters
        assert_eq!(fields[0].length, 9);
    }

    // -- Tab forward --------------------------------------------------------

    #[test]
    fn test_tab_forward() {
        let mut screen = ScreenBuffer5250::new(24, 80);
        let fcw_input = FieldControlWord::from_bytes(0x00, 0x00);
        let fcw_protected = FieldControlWord::from_bytes(0x80, 0x00);

        let record = make_record(
            OpCode::WriteToDisplay,
            vec![
                DataStreamItem5250::Order(Order::Sba(0, 0)),
                DataStreamItem5250::Order(Order::Sf(fcw_protected)),
                DataStreamItem5250::Order(Order::Sba(0, 20)),
                DataStreamItem5250::Order(Order::Sf(fcw_input)),
                DataStreamItem5250::Order(Order::Sba(0, 40)),
                DataStreamItem5250::Order(Order::Sf(fcw_input)),
            ],
        );
        screen.apply_record(&record);

        // Cursor starts at (0,0). Tab should skip the protected field.
        assert!(screen.tab_forward());
        assert_eq!(screen.cursor_pos(), (0, 21)); // first data pos of field at (0,20)
    }

    // -- Tab backward -------------------------------------------------------

    #[test]
    fn test_tab_backward() {
        let mut screen = ScreenBuffer5250::new(24, 80);
        let fcw_input = FieldControlWord::from_bytes(0x00, 0x00);

        let record = make_record(
            OpCode::WriteToDisplay,
            vec![
                DataStreamItem5250::Order(Order::Sba(0, 10)),
                DataStreamItem5250::Order(Order::Sf(fcw_input)),
                DataStreamItem5250::Order(Order::Sba(0, 30)),
                DataStreamItem5250::Order(Order::Sf(fcw_input)),
                // Place cursor after the second field.
                DataStreamItem5250::Order(Order::Sba(0, 35)),
                DataStreamItem5250::Order(Order::Ic),
            ],
        );
        screen.apply_record(&record);

        assert_eq!(screen.cursor_pos(), (0, 35));
        assert!(screen.tab_backward());
        // Should move to the first data position of the field at (0,10).
        assert_eq!(screen.cursor_pos(), (0, 11));
    }

    // -- Type character -----------------------------------------------------

    #[test]
    fn test_type_character() {
        let mut screen = ScreenBuffer5250::new(24, 80);
        let fcw_input = FieldControlWord::from_bytes(0x00, 0x00);

        let record = make_record(
            OpCode::WriteToDisplay,
            vec![
                DataStreamItem5250::Order(Order::Sba(0, 5)),
                DataStreamItem5250::Order(Order::Sf(fcw_input)),
                // Position cursor at first data position.
                DataStreamItem5250::Order(Order::Sba(0, 6)),
                DataStreamItem5250::Order(Order::Ic),
            ],
        );
        screen.apply_record(&record);

        assert!(screen.type_character('A'));
        assert_eq!(screen.get_cell(0, 6).unwrap().character, 'A');
        assert_eq!(screen.cursor_pos(), (0, 7)); // advanced

        // Field should now be marked as modified.
        assert!(screen.get_fields()[0].modified);
    }

    #[test]
    fn test_type_in_protected_field_rejected() {
        let mut screen = ScreenBuffer5250::new(24, 80);
        let fcw_protected = FieldControlWord::from_bytes(0x80, 0x00);

        let record = make_record(
            OpCode::WriteToDisplay,
            vec![
                DataStreamItem5250::Order(Order::Sba(0, 5)),
                DataStreamItem5250::Order(Order::Sf(fcw_protected)),
                DataStreamItem5250::Order(Order::Sba(0, 6)),
                DataStreamItem5250::Order(Order::Ic),
            ],
        );
        screen.apply_record(&record);

        assert!(!screen.type_character('X'));
        assert_eq!(screen.get_cell(0, 6).unwrap().character, ' ');
    }

    #[test]
    fn test_type_numeric_only_rejects_alpha() {
        let mut screen = ScreenBuffer5250::new(24, 80);
        // NumericOnly: shift bits = 011 -> 0x0C
        let fcw_num = FieldControlWord::from_bytes(0x0C, 0x00);

        let record = make_record(
            OpCode::WriteToDisplay,
            vec![
                DataStreamItem5250::Order(Order::Sba(0, 5)),
                DataStreamItem5250::Order(Order::Sf(fcw_num)),
                DataStreamItem5250::Order(Order::Sba(0, 6)),
                DataStreamItem5250::Order(Order::Ic),
            ],
        );
        screen.apply_record(&record);

        assert!(!screen.type_character('A')); // rejected
        assert!(screen.type_character('5')); // accepted
        assert_eq!(screen.get_cell(0, 6).unwrap().character, '5');
    }

    // -- Read modified fields -----------------------------------------------

    #[test]
    fn test_read_modified_fields() {
        let mut screen = ScreenBuffer5250::new(24, 80);
        let fcw_input = FieldControlWord::from_bytes(0x00, 0x00);
        let fcw_protected = FieldControlWord::from_bytes(0x80, 0x00);

        let record = make_record(
            OpCode::WriteToDisplay,
            vec![
                DataStreamItem5250::Order(Order::Sba(0, 0)),
                DataStreamItem5250::Order(Order::Sf(fcw_input)),
                DataStreamItem5250::Order(Order::Sba(0, 10)),
                DataStreamItem5250::Order(Order::Sf(fcw_protected)),
                // Position cursor in the first field.
                DataStreamItem5250::Order(Order::Sba(0, 1)),
                DataStreamItem5250::Order(Order::Ic),
            ],
        );
        screen.apply_record(&record);

        // Type "HI" into the first field.
        screen.type_character('H');
        screen.type_character('I');

        let modified = screen.read_modified_fields();
        assert_eq!(modified.len(), 1);
        assert_eq!(modified[0].0, 0); // row
        assert_eq!(modified[0].1, 0); // col

        // Decode the EBCDIC data back to verify.
        let text = ebcdic::decode_string(&modified[0].2, DEFAULT_CODE_PAGE);
        // The field is 9 chars wide; "HI" followed by 7 spaces.
        assert!(text.starts_with("HI"));
    }

    // -- Read field ---------------------------------------------------------

    #[test]
    fn test_read_field() {
        let mut screen = ScreenBuffer5250::new(24, 80);
        let fcw_input = FieldControlWord::from_bytes(0x00, 0x00);

        // Write "TEST" into a field.
        let record = make_record(
            OpCode::WriteToDisplay,
            vec![
                DataStreamItem5250::Order(Order::Sba(0, 0)),
                DataStreamItem5250::Order(Order::Sf(fcw_input)),
                DataStreamItem5250::Character(0xE3), // T
                DataStreamItem5250::Character(0xC5), // E
                DataStreamItem5250::Character(0xE2), // S
                DataStreamItem5250::Character(0xE3), // T
                DataStreamItem5250::Order(Order::Sba(0, 20)),
                DataStreamItem5250::Order(Order::Sf(fcw_input)),
            ],
        );
        screen.apply_record(&record);

        let text = screen.read_field(0).unwrap();
        assert!(text.starts_with("TEST"));
    }

    // -- Screen text --------------------------------------------------------

    #[test]
    fn test_screen_text() {
        let mut screen = ScreenBuffer5250::new(3, 10);
        let record = make_record(
            OpCode::WriteToDisplay,
            vec![
                DataStreamItem5250::Order(Order::Sba(0, 0)),
                DataStreamItem5250::Character(0xC1), // A
                DataStreamItem5250::Character(0xC2), // B
            ],
        );
        screen.apply_record(&record);

        let text = screen.screen_text();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3);
        assert!(lines[0].starts_with("AB"));
    }

    // -- ClearFormatTable ---------------------------------------------------

    #[test]
    fn test_clear_format_table() {
        let mut screen = ScreenBuffer5250::new(24, 80);
        let fcw = FieldControlWord::from_bytes(0x00, 0x00);

        let record = make_record(
            OpCode::WriteToDisplay,
            vec![
                DataStreamItem5250::Order(Order::Sba(0, 5)),
                DataStreamItem5250::Order(Order::Sf(fcw)),
            ],
        );
        screen.apply_record(&record);
        assert_eq!(screen.get_fields().len(), 1);

        let clear = make_record(OpCode::ClearFormatTable, vec![]);
        screen.apply_record(&clear);
        assert!(screen.get_fields().is_empty());
        // field_start flag should be cleared.
        assert!(!screen.get_cell(0, 5).unwrap().field_start);
    }

    // -- Transparent Data ---------------------------------------------------

    #[test]
    fn test_transparent_data() {
        let mut screen = ScreenBuffer5250::new(24, 80);
        let record = make_record(
            OpCode::WriteToDisplay,
            vec![
                DataStreamItem5250::Order(Order::Sba(0, 0)),
                DataStreamItem5250::Order(Order::Td(vec![0xC8, 0xC5, 0xD3, 0xD3, 0xD6])),
            ],
        );
        screen.apply_record(&record);

        assert_eq!(screen.get_cell(0, 0).unwrap().character, 'H');
        assert_eq!(screen.get_cell(0, 1).unwrap().character, 'E');
        assert_eq!(screen.get_cell(0, 2).unwrap().character, 'L');
        assert_eq!(screen.get_cell(0, 3).unwrap().character, 'L');
        assert_eq!(screen.get_cell(0, 4).unwrap().character, 'O');
    }

    // -- WEA (extended attribute) -------------------------------------------

    #[test]
    fn test_wea_foreground_color() {
        let mut screen = ScreenBuffer5250::new(24, 80);
        let record = make_record(
            OpCode::WriteToDisplay,
            vec![
                DataStreamItem5250::Order(Order::Sba(0, 0)),
                DataStreamItem5250::Order(Order::Wea(ExtendedAttribute {
                    attr_type: 0x01,
                    attr_value: 0x04, // Red
                })),
            ],
        );
        screen.apply_record(&record);
        assert_eq!(screen.get_cell(0, 0).unwrap().foreground, Color5250::Red);
    }

    #[test]
    fn test_wea_underline() {
        let mut screen = ScreenBuffer5250::new(24, 80);
        let record = make_record(
            OpCode::WriteToDisplay,
            vec![
                DataStreamItem5250::Order(Order::Sba(1, 5)),
                DataStreamItem5250::Order(Order::Wea(ExtendedAttribute {
                    attr_type: 0x03,
                    attr_value: 0x01,
                })),
            ],
        );
        screen.apply_record(&record);
        assert!(screen.get_cell(1, 5).unwrap().underline);
    }

    // -- Color mapping ------------------------------------------------------

    #[test]
    fn test_color_mapping() {
        assert_eq!(color_from_5250_attribute(0x00), Color5250::Black);
        assert_eq!(color_from_5250_attribute(0x01), Color5250::Blue);
        assert_eq!(color_from_5250_attribute(0x02), Color5250::Green);
        assert_eq!(color_from_5250_attribute(0x03), Color5250::Turquoise);
        assert_eq!(color_from_5250_attribute(0x04), Color5250::Red);
        assert_eq!(color_from_5250_attribute(0x05), Color5250::Pink);
        assert_eq!(color_from_5250_attribute(0x06), Color5250::Yellow);
        assert_eq!(color_from_5250_attribute(0x07), Color5250::White);
        assert_eq!(color_from_5250_attribute(0x0F), Color5250::White);
        // High nibble should be ignored.
        assert_eq!(color_from_5250_attribute(0xF4), Color5250::Red);
    }

    // -- Full round-trip: parse bytes then apply to screen ------------------

    #[test]
    fn test_parse_and_apply_roundtrip() {
        // Build a raw WTD record: SBA(1,1), "OK" in EBCDIC, IC
        let payload = [
            0x10, 0x01, 0x01, // SBA row=1, col=1 (wire: 1-based)
            0xD6, // 'O'
            0xD2, // 'K'
            0x29, // IC
        ];
        let total = 5 + payload.len();
        let mut raw = Vec::with_capacity(total);
        raw.push((total >> 8) as u8);
        raw.push((total & 0xFF) as u8);
        raw.push(0x04); // record type
        raw.push(0x00); // reserved
        raw.push(0x01); // opcode: WTD
        raw.extend_from_slice(&payload);

        let record = parse_5250_record(&raw).unwrap();
        let mut screen = ScreenBuffer5250::new(24, 80);
        screen.apply_record(&record);

        assert_eq!(screen.get_cell(0, 0).unwrap().character, 'O');
        assert_eq!(screen.get_cell(0, 1).unwrap().character, 'K');
        assert_eq!(screen.cursor_pos(), (0, 2)); // IC was at position after 'K' was written
    }

    // -- SOH order ----------------------------------------------------------

    #[test]
    fn test_soh_does_not_crash() {
        let mut screen = ScreenBuffer5250::new(24, 80);
        let record = make_record(
            OpCode::WriteToDisplay,
            vec![DataStreamItem5250::Order(Order::Soh(SohData {
                length: 3,
                data: vec![0x01, 0x02, 0x03],
            }))],
        );
        // SOH should be a no-op for screen content.
        screen.apply_record(&record);
        assert_eq!(screen.get_cell(0, 0).unwrap().character, ' ');
    }

    // -- Tab with no fields -------------------------------------------------

    #[test]
    fn test_tab_forward_no_fields() {
        let mut screen = ScreenBuffer5250::new(24, 80);
        assert!(!screen.tab_forward());
    }

    #[test]
    fn test_tab_backward_no_fields() {
        let mut screen = ScreenBuffer5250::new(24, 80);
        assert!(!screen.tab_backward());
    }

    // -- Tab with all protected fields --------------------------------------

    #[test]
    fn test_tab_all_protected() {
        let mut screen = ScreenBuffer5250::new(24, 80);
        let fcw_protected = FieldControlWord::from_bytes(0x80, 0x00);

        let record = make_record(
            OpCode::WriteToDisplay,
            vec![
                DataStreamItem5250::Order(Order::Sba(0, 0)),
                DataStreamItem5250::Order(Order::Sf(fcw_protected)),
            ],
        );
        screen.apply_record(&record);
        assert!(!screen.tab_forward());
    }

    // -- 27x132 screen ------------------------------------------------------

    #[test]
    fn test_27x132_screen() {
        let screen = ScreenBuffer5250::new(27, 132);
        assert_eq!(screen.rows(), 27);
        assert_eq!(screen.cols(), 132);
        assert!(screen.get_cell(26, 131).is_some());
        assert!(screen.get_cell(27, 0).is_none());
    }
}
