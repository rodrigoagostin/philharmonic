/// Turn free text into a short, safe name for branches and folders.
///
/// # LEARN #1 — strings and iterators
///
/// Rules (the tests below check each one):
/// 1. Lowercase everything.
/// 2. Keep ASCII letters and digits. Every other character becomes `-`.
/// 3. Never two `-` in a row.
/// 4. No `-` at the start or the end.
/// 5. At most `max_len` characters (and still no `-` at the end after cutting).
/// 6. If nothing is left, return `"task"`.
///
/// Hints:
/// - `text.chars()` gives you an iterator over characters.
/// - `char` has `.is_ascii_alphanumeric()` and `.to_ascii_lowercase()`.
/// - A `String` you build with `String::new()` + `.push(c)` is the easy way.
///   `.ends_with('-')` tells you what you pushed last.
/// - `.trim_matches('-')` returns a `&str`; turn it back into a `String` with `.to_string()`.
/// - For rule 5: `.chars().take(max_len).collect::<String>()`.
///
/// Run the tests with: `cargo test -p philharmonic-core slug`
pub fn slugify(text: &str, max_len: usize) -> String {
    let lower_text = text.to_lowercase();
    let mut slug = String::new();
    for t in lower_text.chars() {
        if t.is_ascii_alphanumeric() {
            slug.push(t);
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = slug.trim_matches('-');

    let slug: String = slug.chars().take(max_len).collect();
    let slug = slug.trim_end_matches('-');

    if slug.is_empty() {
        "task".to_string()
    } else {
        slug.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::slugify;

    #[test]
    fn basic_sentence() {
        assert_eq!(
            slugify("Add rate limiting to /auth", 40),
            "add-rate-limiting-to-auth"
        );
    }

    #[test]
    fn collapses_and_trims_separators() {
        assert_eq!(slugify("  Fix   flaky  test!! ", 40), "fix-flaky-test");
    }

    #[test]
    fn cuts_to_max_len_without_trailing_dash() {
        assert_eq!(
            slugify("Migrate users table to UUID", 20),
            "migrate-users-table"
        );
    }

    #[test]
    fn keeps_digits() {
        assert_eq!(slugify("Bump tokio to 1.44", 40), "bump-tokio-to-1-44");
    }

    #[test]
    fn empty_result_falls_back_to_task() {
        assert_eq!(slugify("!!!", 40), "task");
        assert_eq!(slugify("", 40), "task");
    }
}
