//! What a previous run got through, so a second one does not start over.
//!
//! Deliberately thin. The authority on whether something is installed is its
//! probe, never this file. What a probe cannot recover is a *choice*: declining
//! the tunnel leaves no trace on the machine, so without this a re-run would
//! offer it again as though nobody had said no.
//!
//! Capabilities are what get recorded, because capabilities are what the wizard
//! asks about. Remembering components would store an answer to a question
//! nobody was asked.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// One line per capability: `<id> <answer> <unix-seconds>`. A flat text file
/// rather than JSON so a person can read it, and so a corrupt line costs one
/// entry instead of the whole record.
pub struct Answers {
    path: PathBuf,
    answers: BTreeMap<String, String>,
}

impl Answers {
    pub fn load(data_root: &Path) -> Self {
        let path = data_root.join(".setup-answers");
        let mut answers = BTreeMap::new();
        if let Ok(text) = std::fs::read_to_string(&path) {
            for line in text.lines() {
                let mut parts = line.split_whitespace();
                if let (Some(id), Some(answer)) = (parts.next(), parts.next()) {
                    answers.insert(id.to_string(), answer.to_string());
                }
            }
        }
        Answers { path, answers }
    }

    /// Every recorded answer. A capability absent from the map was never asked,
    /// which is not the same as declined — conflating the two would silently
    /// switch off a capability added since the last run.
    pub fn all(&self) -> BTreeMap<String, bool> {
        self.answers
            .iter()
            .map(|(id, a)| (id.clone(), a == "wanted"))
            .collect()
    }

    pub fn record(&mut self, feature_id: &str, wanted: bool, now_secs: u64) {
        self.answers.insert(
            feature_id.to_string(),
            if wanted {
                "wanted".into()
            } else {
                "declined".into()
            },
        );
        let body: String = self
            .answers
            .iter()
            .map(|(id, answer)| format!("{id} {answer} {now_secs}\n"))
            .collect();
        // Best effort: failing to remember an answer is a worse experience next
        // run, not a reason to fail this one.
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&self.path, body);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_choice_survives_a_reload_and_an_unrecorded_one_stays_unknown() {
        let dir = std::env::temp_dir().join("magician-setup-answers-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let mut answers = Answers::load(&dir);
        assert!(answers.all().is_empty(), "nothing recorded yet");
        answers.record("whatsapp", false, 1_700_000_000);
        answers.record("notes", true, 1_700_000_000);

        let reloaded = Answers::load(&dir);
        assert_eq!(reloaded.all().get("whatsapp"), Some(&false));
        assert_eq!(reloaded.all().get("notes"), Some(&true));
        // None, not false: never asked is different from turned down, and
        // conflating them would silently drop a new capability from the list.
        assert_eq!(reloaded.all().get("never-seen"), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_corrupt_line_costs_one_entry_not_the_file() {
        let dir = std::env::temp_dir().join("magician-setup-answers-corrupt");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(".setup-answers"), "garbage\nwhatsapp declined 1\n").unwrap();

        let answers = Answers::load(&dir);
        assert_eq!(
            answers.all().get("whatsapp"),
            Some(&false),
            "the good line still reads"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_data_root_reads_as_no_answers_rather_than_failing() {
        // First run on a machine where the data root does not exist yet.
        let answers = Answers::load(Path::new("/nonexistent/magician-root"));
        assert!(answers.all().is_empty());
    }
}
