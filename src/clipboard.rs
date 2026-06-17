/// Normalize single-field copy payloads.
///
/// UI labels often wrap or contain incidental spacing. Single values such as
/// public keys, endpoints, addresses, and AllowedIPs should copy cleanly without
/// leading spaces, trailing spaces, or accidental newlines. Raw configs/logs do
/// not use this helper because their newlines are meaningful.
pub fn normalize_single_field_copy_value(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::normalize_single_field_copy_value;

    #[test]
    fn trims_accidental_outer_whitespace() {
        assert_eq!(normalize_single_field_copy_value(" abc "), "abc");
        assert_eq!(normalize_single_field_copy_value("\nabc\n"), "abc");
        assert_eq!(normalize_single_field_copy_value("abc\n"), "abc");
        assert_eq!(normalize_single_field_copy_value("  abc\n  "), "abc");
        assert_eq!(normalize_single_field_copy_value("abc\r\n"), "abc");
    }

    #[test]
    fn joins_display_wrapped_fields() {
        assert_eq!(
            normalize_single_field_copy_value("  one\n  two  \n"),
            "one two"
        );
    }
}
