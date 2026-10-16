//! The one `archived=` query parameter every list endpoint shares.
//!
//! Archiving is a soft state on chat sessions, chat threads, VibeDev projects,
//! tasks and internal tasks (VibeDev runs are internal tasks). Each listing
//! picks its own default so existing callers see exactly what they saw before:
//! chat and VibeDev project lists always returned archived rows, the task and
//! run lists never did.

/// `archived=exclude|include|only` on a list request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchivedFilter {
    /// Live rows only.
    Exclude,
    /// Live and archived rows together.
    Include,
    /// Archived rows only.
    Only,
}

impl ArchivedFilter {
    pub const WIRE_VALUES: [&'static str; 3] = ["exclude", "include", "only"];

    /// Parse the query value. Absent or blank means the listing's `default`; an
    /// unknown value is an error rather than a silent default, so a mistyped
    /// filter never serves the wrong rows under the banner of the right ones.
    pub fn parse(raw: Option<&str>, default: Self) -> Result<Self, String> {
        match raw.map(str::trim).filter(|value| !value.is_empty()) {
            None => Ok(default),
            Some(value) => match value.to_ascii_lowercase().as_str() {
                "exclude" => Ok(Self::Exclude),
                "include" => Ok(Self::Include),
                "only" => Ok(Self::Only),
                _ => Err(format!(
                    "unknown archived filter {value:?}; expected one of {:?}",
                    Self::WIRE_VALUES
                )),
            },
        }
    }

    /// Whether a row whose archived state is `archived` belongs in the list.
    pub fn admits(self, archived: bool) -> bool {
        match self {
            Self::Exclude => !archived,
            Self::Include => true,
            Self::Only => archived,
        }
    }

    /// The archived state a row must have, or `None` when either is admitted.
    pub fn required_state(self) -> Option<bool> {
        match self {
            Self::Exclude => Some(false),
            Self::Include => None,
            Self::Only => Some(true),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ArchivedFilter;

    #[test]
    fn absent_value_keeps_the_listing_default() {
        assert_eq!(
            ArchivedFilter::parse(None, ArchivedFilter::Include),
            Ok(ArchivedFilter::Include)
        );
        assert_eq!(
            ArchivedFilter::parse(Some("  "), ArchivedFilter::Exclude),
            Ok(ArchivedFilter::Exclude)
        );
    }

    #[test]
    fn values_parse_and_unknown_is_refused() {
        assert_eq!(
            ArchivedFilter::parse(Some("Only"), ArchivedFilter::Exclude),
            Ok(ArchivedFilter::Only)
        );
        assert!(ArchivedFilter::parse(Some("archived"), ArchivedFilter::Exclude).is_err());
    }

    #[test]
    fn admits_matches_each_mode() {
        assert!(ArchivedFilter::Exclude.admits(false) && !ArchivedFilter::Exclude.admits(true));
        assert!(ArchivedFilter::Include.admits(false) && ArchivedFilter::Include.admits(true));
        assert!(!ArchivedFilter::Only.admits(false) && ArchivedFilter::Only.admits(true));
        assert_eq!(ArchivedFilter::Include.required_state(), None);
        assert_eq!(ArchivedFilter::Only.required_state(), Some(true));
    }
}
