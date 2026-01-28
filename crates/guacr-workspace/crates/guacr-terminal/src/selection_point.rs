/// Selection point with side tracking for pixel-perfect text selection
///
/// This module implements the selection point tracking system inspired by
/// KCM's GUACAMOLE-2117 improvements. It tracks which side of a character
/// the mouse is on (left vs right) to provide accurate selection boundaries.
use crate::TerminalEmulator;

/// Which side of a terminal column the selection point is on
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColumnSide {
    /// Left side of the column (left half of character)
    Left,
    /// Right side of the column (right half of character)
    Right,
}

/// A precise selection point within the terminal
///
/// Tracks not just row/column, but also which side of the character
/// and the character's width for accurate selection boundaries.
#[derive(Debug, Clone, PartialEq)]
pub struct SelectionPoint {
    /// The row value of the point
    pub row: u16,
    /// The column value of the point
    pub column: u16,
    /// The specific side of the column pointed to
    pub side: ColumnSide,
    /// The starting column of the character pointed to
    pub char_starting_column: u16,
    /// The width of the character pointed to (1 for ASCII, 2+ for wide chars)
    pub char_width: u16,
}

impl SelectionPoint {
    /// Create a new selection point from pixel coordinates
    ///
    /// # Arguments
    /// * `x_px` - X coordinate in pixels
    /// * `y_px` - Y coordinate in pixels
    /// * `char_width` - Width of a character cell in pixels
    /// * `char_height` - Height of a character cell in pixels
    /// * `cols` - Number of columns in terminal
    /// * `rows` - Number of rows in terminal
    /// * `terminal` - Terminal emulator to query character information
    pub fn from_pixel_coords(
        x_px: u32,
        y_px: u32,
        char_width: u32,
        char_height: u32,
        cols: u16,
        rows: u16,
        terminal: &TerminalEmulator,
    ) -> Self {
        // Convert to cell coordinates
        let col = (x_px / char_width).min(cols as u32 - 1) as u16;
        let row = (y_px / char_height).min(rows as u32 - 1) as u16;

        // Determine which side of the character (left or right half)
        let char_x_offset = x_px % char_width;
        let side = if char_x_offset < (char_width / 2) {
            ColumnSide::Left
        } else {
            ColumnSide::Right
        };

        // Find the actual character at this position
        let (char_starting_column, char_width_cells) = find_char_at_position(terminal, row, col);

        SelectionPoint {
            row,
            column: col,
            side,
            char_starting_column,
            char_width: char_width_cells,
        }
    }

    /// Create a selection point directly from row/column/side
    ///
    /// This is useful for programmatic selection (e.g., double-click word selection)
    pub fn new(row: u16, column: u16, side: ColumnSide, terminal: &TerminalEmulator) -> Self {
        let (char_starting_column, char_width) = find_char_at_position(terminal, row, column);

        SelectionPoint {
            row,
            column,
            side,
            char_starting_column,
            char_width,
        }
    }

    /// Determine if this point comes after another point
    ///
    /// Uses left-to-right, top-to-bottom ordering. A point is "after"
    /// if it's further right or down from the other point.
    ///
    /// # Arguments
    /// * `other` - Point to compare against
    ///
    /// # Returns
    /// `true` if this point comes after the other point
    pub fn is_after(&self, other: &SelectionPoint) -> bool {
        if self.row != other.row {
            return self.row > other.row;
        }
        if self.column != other.column {
            return self.column > other.column;
        }
        if self.side != other.side {
            // If same column, right side is after left side
            return self.side == ColumnSide::Right;
        }
        // Points are identical
        false
    }

    /// Round the column up to the next character boundary
    ///
    /// If we're anywhere except the leftmost position in a character,
    /// return the starting column of the next character.
    ///
    /// # Returns
    /// Column value for the start of the selection
    pub fn round_up(&self) -> u16 {
        if self.column == self.char_starting_column && self.side == ColumnSide::Left {
            // Already at the leftmost position
            self.column
        } else {
            // Round up to next character
            self.char_starting_column + self.char_width
        }
    }

    /// Round the column down to the previous character boundary
    ///
    /// If we're anywhere except the rightmost position in a character,
    /// return the ending column of the previous character.
    ///
    /// # Returns
    /// Column value for the end of the selection
    pub fn round_down(&self) -> u16 {
        let end_char_column = self.char_starting_column + self.char_width - 1;
        if self.column == end_char_column && self.side == ColumnSide::Right {
            // Already at the rightmost position
            end_char_column
        } else {
            // Round down to previous character
            if self.char_starting_column > 0 {
                self.char_starting_column - 1
            } else {
                0
            }
        }
    }
}

/// Determine if two selection points enclose at least one character
///
/// A range encloses text if it stretches from the furthest left to
/// furthest right of any complete character.
///
/// # Arguments
/// * `start` - Starting point (must come before end)
/// * `end` - Ending point (must come after start)
///
/// # Returns
/// `true` if the range contains at least one complete character
pub fn points_enclose_text(start: &SelectionPoint, end: &SelectionPoint) -> bool {
    // Different rows always contain at least one character
    if start.row != end.row {
        return true;
    }

    // Check if the starting point completely contains a character
    let start_char_end = start.char_starting_column + start.char_width - 1;
    if start.side == ColumnSide::Left
        && start.column == start.char_starting_column
        && (end.column > start_char_end
            || (end.column == start_char_end && end.side == ColumnSide::Right))
    {
        return true;
    }

    // Check the next character after start
    let end_char_end = end.char_starting_column + end.char_width - 1;
    let second_char_start = start_char_end + 1;
    if second_char_start < end.char_starting_column
        || (second_char_start == end.char_starting_column
            && end.column == end_char_end
            && end.side == ColumnSide::Right)
    {
        return true;
    }

    false
}

/// Find the character at a given position and return its starting column and width
///
/// # Arguments
/// * `terminal` - Terminal emulator to query
/// * `row` - Row to search
/// * `column` - Column to search
///
/// # Returns
/// Tuple of (starting_column, width) for the character at this position
fn find_char_at_position(terminal: &TerminalEmulator, row: u16, column: u16) -> (u16, u16) {
    let screen = terminal.screen();

    // Get the cell at this position
    if let Some(cell) = screen.cell(row, column) {
        let contents = cell.contents();

        // Check if this is a wide character
        if contents.chars().count() > 0 {
            let first_char = contents.chars().next().unwrap();
            let width = unicode_width::UnicodeWidthChar::width(first_char).unwrap_or(1) as u16;

            // If width > 1, we need to find the starting column
            if width > 1 {
                // Check if we're on a continuation cell
                // Scan backwards to find the start
                let mut start_col = column;
                while start_col > 0 {
                    if let Some(prev_cell) = screen.cell(row, start_col - 1) {
                        let prev_contents = prev_cell.contents();
                        if prev_contents.chars().count() > 0 {
                            let prev_char = prev_contents.chars().next().unwrap();
                            let prev_width =
                                unicode_width::UnicodeWidthChar::width(prev_char).unwrap_or(1);
                            if prev_width > 1 && prev_contents == contents {
                                // This is part of the same wide character
                                start_col -= 1;
                                continue;
                            }
                        }
                    }
                    break;
                }
                return (start_col, width);
            }

            return (column, width);
        }
    }

    // Default: single-width character
    (column, 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_column_side_ordering() {
        // Left comes before Right on same column
        assert!(!(ColumnSide::Left == ColumnSide::Right));
    }

    #[test]
    fn test_point_is_after_same_point() {
        let terminal = create_test_terminal();
        let point1 = SelectionPoint::new(1, 1, ColumnSide::Left, &terminal);
        let point2 = SelectionPoint::new(1, 1, ColumnSide::Left, &terminal);

        // Same point is not after itself
        assert!(!point1.is_after(&point2));
        assert!(!point2.is_after(&point1));
    }

    #[test]
    fn test_point_is_after_different_rows() {
        let terminal = create_test_terminal();
        let point1 = SelectionPoint::new(1, 1, ColumnSide::Left, &terminal);
        let point2 = SelectionPoint::new(2, 1, ColumnSide::Left, &terminal);

        assert!(!point1.is_after(&point2));
        assert!(point2.is_after(&point1));
    }

    #[test]
    fn test_point_is_after_different_columns() {
        let terminal = create_test_terminal();
        let point1 = SelectionPoint::new(1, 1, ColumnSide::Left, &terminal);
        let point2 = SelectionPoint::new(1, 2, ColumnSide::Left, &terminal);

        assert!(!point1.is_after(&point2));
        assert!(point2.is_after(&point1));
    }

    #[test]
    fn test_point_is_after_different_sides() {
        let terminal = create_test_terminal();
        let point1 = SelectionPoint::new(1, 1, ColumnSide::Left, &terminal);
        let point2 = SelectionPoint::new(1, 1, ColumnSide::Right, &terminal);

        assert!(!point1.is_after(&point2));
        assert!(point2.is_after(&point1));
    }

    #[test]
    fn test_round_up() {
        let _terminal = create_test_terminal();

        // Left side of column - stays at column
        let point = SelectionPoint {
            row: 1,
            column: 1,
            side: ColumnSide::Left,
            char_starting_column: 1,
            char_width: 1,
        };
        assert_eq!(point.round_up(), 1);

        // Right side of column - rounds to next column
        let point = SelectionPoint {
            row: 1,
            column: 1,
            side: ColumnSide::Right,
            char_starting_column: 1,
            char_width: 1,
        };
        assert_eq!(point.round_up(), 2);
    }

    #[test]
    fn test_round_down() {
        let _terminal = create_test_terminal();

        // Right side of column - stays at column
        let point = SelectionPoint {
            row: 1,
            column: 1,
            side: ColumnSide::Right,
            char_starting_column: 1,
            char_width: 1,
        };
        assert_eq!(point.round_down(), 1);

        // Left side of column - rounds to previous column
        let point = SelectionPoint {
            row: 1,
            column: 1,
            side: ColumnSide::Left,
            char_starting_column: 1,
            char_width: 1,
        };
        assert_eq!(point.round_down(), 0);
    }

    #[test]
    fn test_points_enclose_text_different_rows() {
        let terminal = create_test_terminal();
        let start = SelectionPoint::new(1, 1, ColumnSide::Left, &terminal);
        let end = SelectionPoint::new(2, 1, ColumnSide::Left, &terminal);

        assert!(points_enclose_text(&start, &end));
    }

    #[test]
    fn test_points_enclose_text_same_position() {
        let terminal = create_test_terminal();
        let start = SelectionPoint::new(1, 1, ColumnSide::Left, &terminal);
        let end = SelectionPoint::new(1, 1, ColumnSide::Left, &terminal);

        assert!(!points_enclose_text(&start, &end));
    }

    // Helper function to create a test terminal
    fn create_test_terminal() -> TerminalEmulator {
        TerminalEmulator::new_with_scrollback(24, 80, 1000)
    }
}
