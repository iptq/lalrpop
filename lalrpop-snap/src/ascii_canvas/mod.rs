//! An "ASCII Canvas" allows us to draw lines and write text into a
//! fixed-sized canvas and then convert that canvas into ASCII
//! characters. ANSI styling is supported.

use std::cmp;
use std::ops::Range;
use std::iter::ExactSizeIterator;
use style::Style;
use term::{self, Terminal};

mod row;
#[cfg(test)] mod test;

pub use self::row::Row;

///////////////////////////////////////////////////////////////////////////

/// AsciiView is a view onto an `AsciiCanvas` which potentially
/// applies transformations along the way (e.g., shifting, adding
/// styling information). Most of the main drawing methods for
/// `AsciiCanvas` are defined as inherent methods on an `AsciiView`
/// trait object.
pub trait AsciiView {
    fn columns(&self) -> usize;
    fn read_char(&mut self, row: usize, column: usize) -> char;
    fn write_char(&mut self, row: usize, column: usize, ch: char, style: Style);
}

impl<'a> AsciiView+'a {
    fn add_box_dirs(&mut self,
                    row: usize,
                    column: usize,
                    dirs: u8)
    {
        let old_ch = self.read_char(row, column);
        let new_ch = add_dirs(old_ch, dirs);
        self.write_char(row, column, new_ch, Style::new());
    }

    pub fn draw_vertical_line(&mut self,
                              rows: Range<usize>,
                              column: usize)
    {
        let len = rows.len();
        for (index, r) in rows.enumerate() {
            let new_dirs = if index == 0 {
                DOWN
            } else if index == len - 1 {
                UP
            } else {
                UP | DOWN
            };
            self.add_box_dirs(r, column, new_dirs);
        }
    }

    pub fn draw_horizontal_line(&mut self,
                                row: usize,
                                columns: Range<usize>)
    {
        let len = columns.len();
        for (index, c) in columns.enumerate() {
            let new_dirs = if index == 0 {
                RIGHT
            } else if index == len - 1 {
                LEFT
            } else {
                LEFT | RIGHT
            };
            self.add_box_dirs(row, c, new_dirs);
        }
    }

    pub fn write_chars<I>(&mut self,
                          row: usize,
                          column: usize,
                          chars: I,
                          style: Style)
        where I: Iterator<Item=char>
    {
        for (i, ch) in chars.enumerate() {
            self.write_char(row, column + i, ch, style);
        }
    }

    pub fn shift<'c>(&'c mut self, row: usize, column: usize) -> ShiftedView<'c> {
        ShiftedView::new(self, row, column)
    }

    pub fn styled<'c>(&'c mut self, style: Style) -> StyleView<'c> {
        StyleView::new(self, style)
    }
}

pub struct AsciiCanvas {
    columns: usize,
    rows: usize,
    characters: Vec<char>,
    styles: Vec<Style>,
}

/// To use an `AsciiCanvas`, first create the canvas, then draw any
/// lines, then write text labels. It is required to draw the lines
/// first so that we can detect intersecting lines properly (we could
/// track which characters belong to lines, I suppose).
impl AsciiCanvas {
    pub fn new(rows: usize, columns: usize) -> Self {
        AsciiCanvas {
            rows: rows,
            columns: columns,
            characters: vec![' '; columns * rows],
            styles: vec![Style::new(); columns * rows],
        }
    }

    fn grow_rows_if_needed(&mut self, new_rows: usize) {
        if new_rows >= self.rows {
            let new_chars = (new_rows - self.rows) * self.columns;
            self.characters.extend((0..new_chars).map(|_| ' '));
            self.styles.extend((0..new_chars).map(|_| Style::new()));
            self.rows = new_rows;
        }
    }

    fn index(&mut self, r: usize, c: usize) -> usize {
        self.grow_rows_if_needed(r + 1);
        self.in_range_index(r, c)
    }

    fn in_range_index(&self, r: usize, c: usize) -> usize {
        assert!(r < self.rows);
        assert!(c <= self.columns);
        r * self.columns + c
    }

    fn start_index(&self, r: usize) -> usize {
        self.in_range_index(r, 0)
    }

    fn end_index(&self, r: usize) -> usize {
        self.in_range_index(r, self.columns)
    }

    pub fn write_to<T:Terminal+?Sized>(&self, term: &mut T) -> term::Result<()> {
        for row in self.to_strings() {
            try!(row.write_to(term));
            try!(writeln!(term, ""));
        }
        Ok(())
    }

    pub fn to_strings(&self) -> Vec<Row> {
        (0..self.rows)
            .map(|row| {
                let start = self.start_index(row);
                let end = self.end_index(row);
                let chars = &self.characters[start..end];
                let styles = &self.styles[start..end];
                Row::new(chars, styles)
            })
            .collect()
    }
}

impl AsciiView for AsciiCanvas {
    fn columns(&self) -> usize {
        self.columns
    }

    fn read_char(&mut self, row: usize, column: usize) -> char {
        assert!(column < self.columns);
        let index = self.index(row, column);
        self.characters[index]
    }

    fn write_char(&mut self,
                  row: usize,
                  column: usize,
                  ch: char,
                  style: Style)
    {
        assert!(column < self.columns);
        let index = self.index(row, column);
        self.characters[index] = ch;
        self.styles[index] = style;
    }
}

#[derive(Copy, Clone)]
struct Point {
    row: usize,
    column: usize,
}

/// Gives a view onto an AsciiCanvas that has a fixed upper-left
/// point.
pub struct ShiftedView<'canvas> {
    // either the base canvas or another view
    base: &'canvas mut AsciiView,

    // fixed at creation: the content is always allowed to grow down,
    // but cannot grow right more than `num_columns`
    upper_left: Point,

    // this is updated to track content that is emitted
    lower_right: Point,
}

impl<'canvas> ShiftedView<'canvas> {
    pub fn new(base: &'canvas mut AsciiView,
               row: usize,
               column: usize)
               -> Self {
        let upper_left = Point { row: row, column: column };
        ShiftedView {
            base: base,
            upper_left: upper_left,
            lower_right: upper_left,
        }
    }

    // Finalize the view and learn how much was written.  What is
    // returned is the *maximal* row/column -- so if you write to that
    // location, you would overwrite some of the content that was
    // written.
    pub fn close(self) -> (usize, usize) {
        (self.lower_right.row, self.lower_right.column)
    }

    fn track_max(&mut self, row: usize, column: usize) {
        self.lower_right.row = cmp::max(self.lower_right.row, row);
        self.lower_right.column = cmp::max(self.lower_right.column, column);
    }
}

impl<'canvas> AsciiView for ShiftedView<'canvas> {
    fn columns(&self) -> usize {
        self.base.columns() - self.upper_left.column
    }

    fn read_char(&mut self, row: usize, column: usize) -> char {
        let row = self.upper_left.row + row;
        let column = self.upper_left.column + column;
        self.base.read_char(row, column)
    }

    fn write_char(&mut self,
                  row: usize,
                  column: usize,
                  ch: char,
                  style: Style)
    {
        let row = self.upper_left.row + row;
        let column = self.upper_left.column + column;
        self.track_max(row, column);
        self.base.write_char(row, column, ch, style)
    }
}

pub struct StyleView<'canvas> {
    base: &'canvas mut AsciiView,
    style: Style,
}

impl<'canvas> StyleView<'canvas> {
    pub fn new(base: &'canvas mut AsciiView, style: Style) -> Self {
        StyleView {
            base: base,
            style: style,
        }
    }
}

impl<'canvas> AsciiView for StyleView<'canvas> {
    fn columns(&self) -> usize {
        self.base.columns()
    }

    fn read_char(&mut self, row: usize, column: usize) -> char {
        self.base.read_char(row, column)
    }

    fn write_char(&mut self,
                  row: usize,
                  column: usize,
                  ch: char,
                  style: Style)
    {
        self.base.write_char(row, column, ch, style.with(self.style))
    }
}

///////////////////////////////////////////////////////////////////////////
// Unicode box-drawing characters

pub const UP: u8 = 0b0001;
pub const DOWN: u8 = 0b0010;
pub const LEFT: u8 = 0b0100;
pub const RIGHT: u8 = 0b1000;

pub const BOX_CHARS: &'static [(char, u8)] = &[
    ('╵', UP),
    ('│', UP | DOWN),
    ('┤', UP | DOWN | LEFT),
    ('├', UP | DOWN | RIGHT),
    ('┼', UP | DOWN | LEFT | RIGHT),
    ('┘', UP | LEFT),
    ('└', UP | RIGHT),
    ('┴', UP | LEFT | RIGHT),

    // No UP:
    ('╷', DOWN),
    ('┐', DOWN | LEFT),
    ('┌', DOWN | RIGHT),
    ('┬', DOWN | LEFT | RIGHT),

    // No UP|DOWN:
    ('╶', LEFT),
    ('─', LEFT | RIGHT),

    // No LEFT:
    ('╴', RIGHT),

    // No RIGHT:
    (' ', 0),
];

fn box_char_for_dirs(dirs: u8) -> char {
    for &(c, d) in BOX_CHARS {
        if dirs == d {
            return c;
        }
    }
    panic!("no box character for dirs: {:b}", dirs);
}

fn dirs_for_box_char(ch: char) -> Option<u8> {
    for &(c, d) in BOX_CHARS {
        if c == ch {
            return Some(d);
        }
    }
    None
}

fn add_dirs(old_ch: char, new_dirs: u8) -> char {
    let old_dirs = dirs_for_box_char(old_ch).unwrap_or(0);
    box_char_for_dirs(old_dirs | new_dirs)
}
