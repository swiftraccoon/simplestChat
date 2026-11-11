//! Text conventions of the helpers this crate replaces.

fn python_line_break(value: char) -> bool {
    matches!(
        value,
        '\n' | '\r'
            | '\u{b}'
            | '\u{c}'
            | '\u{1c}'
            | '\u{1d}'
            | '\u{1e}'
            | '\u{85}'
            | '\u{2028}'
            | '\u{2029}'
    )
}
/// The lines of `value` as Python's `str.splitlines` divides it.
pub(crate) fn python_lines(value: &str) -> impl Iterator<Item = &str> {
    let mut after_cr = false;
    value
        .split_inclusive(python_line_break)
        .filter_map(move |part| {
            let skip = after_cr && part == "\n";
            let last = part.chars().next_back();
            after_cr = last == Some('\r');
            if skip || part.is_empty() {
                return None;
            }
            let end = last
                .filter(|c| python_line_break(*c))
                .map_or(part.len(), |c| part.len() - c.len_utf8());
            Some(&part[..end])
        })
}
