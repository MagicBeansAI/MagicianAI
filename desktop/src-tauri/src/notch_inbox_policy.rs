//! Incoming attention updates badges; opening the drawer is a user action.

pub(crate) fn drawer_open_after_refresh(
    currently_open: bool,
    is_attention: bool,
    previous_count: u32,
    count: u32,
) -> bool {
    currently_open && !(is_attention && previous_count > 0 && count == 0)
}

#[cfg(test)]
mod tests {
    use super::drawer_open_after_refresh;

    #[test]
    fn incoming_questions_never_open_a_closed_drawer() {
        for (before, after) in [(0, 1), (1, 3), (3, 3), (3, 0)] {
            assert!(!drawer_open_after_refresh(false, true, before, after));
        }
    }

    #[test]
    fn manual_drawers_are_preserved_except_when_attention_finishes() {
        for (before, after) in [(0, 0), (0, 1), (1, 3), (3, 3)] {
            assert!(drawer_open_after_refresh(true, true, before, after));
        }
        assert!(!drawer_open_after_refresh(true, true, 3, 0));
        assert!(drawer_open_after_refresh(true, false, 3, 0));
    }
}
