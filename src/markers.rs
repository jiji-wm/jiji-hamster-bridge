//! Continuation-marker stamping for carried-forward fact descriptions.
//!
//! A description is a sequence of blocks. A block's first line is either an
//! explicitly marked line (its left-stripped form starts with `*` or `..`) or
//! the first non-blank line when no block is open yet (an implicit new block).
//! Interior lines — bullets, extension paragraphs — carry no marker and belong
//! to the block above them. Blank lines never open or close a block.
//!
//! When a fact is resumed by cloning a prior fact's description, every block in
//! that description is being carried forward, so each block's first line is
//! rewritten to the continued marker `..`. `*` (new) and unmarked first lines
//! both become `..`; an already-`..` first line is left untouched (idempotent).

/// Rewrite every block's first line in `description` to the continued marker
/// (`..`). Bullets, extension lines, and blank lines are left untouched.
pub fn mark_continuation(description: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut block_open = false;
    for line in description.split('\n') {
        if line.trim().is_empty() {
            out.push(line.to_string());
            continue;
        }
        let stripped = line.trim_start();
        // An explicit marker always opens a fresh block; an unmarked line opens
        // one only when none is currently open (the implicit first block).
        let opens_block = stripped.starts_with("..") || stripped.starts_with('*') || !block_open;
        if opens_block {
            out.push(mark_first_line(line));
            block_open = true;
        } else {
            out.push(line.to_string());
        }
    }
    out.join("\n")
}

/// Rewrite one block-opening line to the continued marker, preserving leading
/// indentation. Already-`..` lines are returned unchanged.
fn mark_first_line(line: &str) -> String {
    let indent_len = line.len() - line.trim_start().len();
    let (indent, rest) = line.split_at(indent_len);
    if rest.starts_with("..") {
        return line.to_string();
    }
    let text = rest.strip_prefix('*').unwrap_or(rest).trim_start();
    format!("{indent}.. {text}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_first_line_gets_continued_marker() {
        assert_eq!(mark_continuation("write report"), ".. write report");
    }

    #[test]
    fn new_marker_is_rewritten_to_continued() {
        assert_eq!(mark_continuation("*write report"), ".. write report");
        assert_eq!(mark_continuation("* write report"), ".. write report");
    }

    #[test]
    fn already_continued_first_line_is_left_unchanged() {
        assert_eq!(mark_continuation(".. write report"), ".. write report");
        assert_eq!(mark_continuation("..write report"), "..write report");
    }

    #[test]
    fn already_marked_multi_block_is_idempotent() {
        // Resuming the same task twice in a day re-runs this over a description
        // already stamped with multiple `..` heads — it must be a no-op.
        let already = ".. write report\n- draft\n.. review pr";
        assert_eq!(mark_continuation(already), already);
    }

    #[test]
    fn bullets_and_extension_lines_are_untouched() {
        let desc = "*write report\n- draft intro\nmore detail";
        assert_eq!(
            mark_continuation(desc),
            ".. write report\n- draft intro\nmore detail"
        );
    }

    #[test]
    fn every_block_first_line_is_marked() {
        let desc = "*write report\n- draft\n*review pr";
        assert_eq!(
            mark_continuation(desc),
            ".. write report\n- draft\n.. review pr"
        );
    }

    #[test]
    fn implicit_first_block_with_body() {
        let desc = "write report\n- draft";
        assert_eq!(mark_continuation(desc), ".. write report\n- draft");
    }

    #[test]
    fn blank_lines_never_open_a_block_and_are_preserved() {
        // The line after the blank belongs to the already-open first block, so
        // it is body, not a new head — only the first line is marked.
        let desc = "write report\n\nmore detail";
        assert_eq!(mark_continuation(desc), ".. write report\n\nmore detail");
    }

    #[test]
    fn leading_blank_lines_are_preserved_then_first_real_line_marked() {
        assert_eq!(mark_continuation("\nwrite report"), "\n.. write report");
    }

    #[test]
    fn leading_indentation_is_preserved() {
        assert_eq!(mark_continuation("  *write report"), "  .. write report");
    }

    #[test]
    fn empty_description_is_unchanged() {
        assert_eq!(mark_continuation(""), "");
    }
}
