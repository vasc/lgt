use anyhow::{Result, bail};

/// Subcommand words can never name a capture: `lgt ls` must always mean the
/// subcommand, never "capture a stream called ls".
const RESERVED_NAMES: [&str; 9] = [
    "tui",
    "ls",
    "cat",
    "rm",
    "clean",
    "archive",
    "unarchive",
    "path",
    "help",
];

/// Validation for capture names, which share the root command line with
/// subcommands.
pub fn validate_stream_name(name: &str) -> Result<()> {
    validate_name(name)?;
    if RESERVED_NAMES.contains(&name) {
        bail!(
            "`{name}` is a reserved command word and cannot be used as a capture name; \
             if you meant the `lgt {name}` subcommand, put options after it"
        );
    }
    Ok(())
}

/// Names are referenced from the command line and shown in the TUI; keep them
/// simple so they never need quoting.
pub fn validate_name(name: &str) -> Result<()> {
    if name.is_empty() {
        bail!("name must not be empty");
    }
    if name.starts_with('-') {
        bail!("name must not start with `-`");
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    {
        bail!("name may only contain letters, digits, `.`, `_` and `-`");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_validation() {
        assert!(validate_name("build.step_1-a").is_ok());
        assert!(validate_name("").is_err());
        assert!(validate_name("-x").is_err());
        assert!(validate_name("a b").is_err());
        assert!(validate_name("a/b").is_err());
    }

    #[test]
    fn stream_names_exclude_command_words() {
        assert!(validate_stream_name("build").is_ok());
        for reserved in RESERVED_NAMES {
            assert!(validate_stream_name(reserved).is_err(), "{reserved}");
        }
        // Group names are not restricted by the reserved list.
        assert!(validate_name("ls").is_ok());
    }
}
