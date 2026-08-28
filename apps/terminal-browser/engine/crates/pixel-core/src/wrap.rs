use std::ops::Range;

use crate::canvas::{char_advance, measure_marked};
use crate::text_input::Mark;

pub(crate) const WRAP_SLACK: f32 = 1.0;

pub fn wrap_lines(
    text: &str,
    font: &fontdue::Font,
    px: f32,
    max_width: Option<f32>,
    marks: &[Mark],
) -> Vec<Range<usize>> {
    let mut lines = Vec::new();
    let mut start = 0;
    loop {
        let end = text[start..].find('\n').map_or(text.len(), |i| start + i);
        match max_width {
            None => lines.push(start..end),
            Some(width) => wrap_logical_line(text, start..end, font, px, width, marks, &mut lines),
        }
        if end == text.len() {
            break;
        }
        start = end + 1;
    }
    lines
}

#[allow(clippy::too_many_arguments)]
fn wrap_logical_line(
    text: &str,
    line: Range<usize>,
    font: &fontdue::Font,
    px: f32,
    max_width: f32,
    marks: &[Mark],
    out: &mut Vec<Range<usize>>,
) {
    if line.is_empty() {
        out.push(line);
        return;
    }
    let mut line_start = line.start;
    let mut pen = 0.0f32;
    let mut cursor = line.start;
    while cursor < line.end {
        let chunk_end = next_break(text, cursor, line.end);
        let word_end = text[cursor..chunk_end]
            .trim_end_matches(' ')
            .len()
            .checked_add(cursor)
            .expect("in bounds");
        let word_w = measure_marked(font, text, cursor..word_end, px, marks);
        if pen > 0.0 && pen + word_w > max_width {
            out.push(line_start..cursor);
            line_start = cursor;
            pen = 0.0;
        }
        if pen == 0.0 && word_w > max_width {
            cursor = break_long_word(
                text,
                cursor,
                chunk_end,
                font,
                px,
                max_width,
                marks,
                &mut line_start,
                out,
            );
            continue;
        }
        pen += measure_marked(font, text, cursor..chunk_end, px, marks);
        cursor = chunk_end;
    }
    out.push(line_start..line.end);
}

fn next_break(text: &str, from: usize, limit: usize) -> usize {
    let bytes = text.as_bytes();
    let mut i = from;
    while i < limit && bytes[i] != b' ' {
        i += 1;
        while i < limit && !text.is_char_boundary(i) {
            i += 1;
        }
    }
    while i < limit && bytes[i] == b' ' {
        i += 1;
    }
    i
}

#[allow(clippy::too_many_arguments)]
fn break_long_word(
    text: &str,
    from: usize,
    limit: usize,
    font: &fontdue::Font,
    px: f32,
    max_width: f32,
    marks: &[Mark],
    line_start: &mut usize,
    out: &mut Vec<Range<usize>>,
) -> usize {
    let mut pen = 0.0f32;
    let mut cursor = from;
    for (i, c) in text[from..limit].char_indices() {
        let advance = char_advance(font, c, from + i, px, marks);
        if pen > 0.0 && pen + advance > max_width {
            out.push(*line_start..from + i);
            *line_start = from + i;
            pen = 0.0;
        }
        pen += advance;
        cursor = from + i + c.len_utf8();
        if pen >= max_width && cursor < limit {
            break;
        }
    }
    cursor
}

pub fn line_of_offset(lines: &[Range<usize>], offset: usize) -> usize {
    lines
        .iter()
        .rposition(|line| line.start <= offset)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::measure_text;

    static FONT_BYTES: &[u8] =
        include_bytes!("../../../assets/fonts/JetBrainsMono-Regular.ttf");

    fn font() -> fontdue::Font {
        fontdue::Font::from_bytes(FONT_BYTES, fontdue::FontSettings::default()).unwrap()
    }

    fn parts<'a>(text: &'a str, lines: &[Range<usize>]) -> Vec<&'a str> {
        lines.iter().map(|r| &text[r.clone()]).collect()
    }

    #[test]
    fn no_max_width_gives_logical_lines() {
        let f = font();
        let lines = wrap_lines("one\ntwo\n\nthree", &f, 16.0, None, &[]);
        assert_eq!(
            parts("one\ntwo\n\nthree", &lines),
            ["one", "two", "", "three"]
        );
    }

    #[test]
    fn wraps_at_word_boundaries_and_covers_every_byte() {
        let f = font();
        let ch = measure_text(&f, "a", 16.0);
        let text = "alpha beta gamma delta";
        let lines = wrap_lines(text, &f, 16.0, Some(ch * 12.0), &[]);
        assert!(lines.len() > 1, "{lines:?}");
        for pair in lines.windows(2) {
            assert_eq!(pair[0].end, pair[1].start, "contiguous coverage");
        }
        assert_eq!(lines[0].start, 0);
        assert_eq!(lines.last().unwrap().end, text.len());
        for line in &lines {
            let visible = text[line.clone()].trim_end_matches(' ');
            assert!(
                measure_text(&f, visible, 16.0) <= ch * 12.0 + 0.5,
                "visible part fits: {:?}",
                &text[line.clone()]
            );
            assert!(!text[line.clone()].starts_with(' '), "no leading spaces");
        }
    }

    #[test]
    fn breaks_overlong_words_by_character() {
        let f = font();
        let ch = measure_text(&f, "a", 16.0);
        let text = "aaaaaaaaaaaaaaaaaaaa";
        let lines = wrap_lines(text, &f, 16.0, Some(ch * 6.0), &[]);
        assert!(lines.len() >= 3, "{lines:?}");
        for line in &lines {
            assert!(line.end - line.start <= 6);
            assert!(line.end > line.start, "always makes progress");
        }
        assert_eq!(lines.last().unwrap().end, text.len());
    }

    #[test]
    fn line_of_offset_puts_boundaries_on_the_next_line() {
        let lines = vec![0..6, 6..11];
        assert_eq!(line_of_offset(&lines, 0), 0);
        assert_eq!(line_of_offset(&lines, 5), 0);
        assert_eq!(
            line_of_offset(&lines, 6),
            1,
            "wrap boundary starts the next line"
        );
        assert_eq!(
            line_of_offset(&lines, 11),
            1,
            "text end stays on the last line"
        );
    }

    #[test]
    fn empty_text_is_one_empty_line() {
        let f = font();
        let lines = wrap_lines("", &f, 16.0, Some(100.0), &[]);
        assert_eq!(lines, vec![0..0]);
    }
}
