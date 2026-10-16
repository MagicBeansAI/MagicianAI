use super::*;

pub(crate) fn sample() -> (
    TemplateContract,
    BTreeMap<String, Vec<u8>>,
    BTreeMap<String, Vec<u8>>,
) {
    let source = br#"{"fact":"Verified facts"}"#.to_vec();
    let template = TemplateContract {
        payload_file: "message.txt".into(),
        slots_file: "slots.json".into(),
        maximum_payload_bytes: 512,
        parts: vec![
            TemplatePart::Literal {
                value: "Hello ".into(),
            },
            TemplatePart::Slot { id: "name".into() },
            TemplatePart::Literal { value: ": ".into() },
            TemplatePart::Slot { id: "fact".into() },
        ],
        slots: BTreeMap::from([
            (
                "name".into(),
                SlotRule::FixedChoices {
                    values: BTreeSet::from(["Alex".into()]),
                    maximum_bytes: 64,
                },
            ),
            (
                "fact".into(),
                SlotRule::SealedSource {
                    source_id: "owner-facts".into(),
                    content_hash: blake3::hash(&source).to_hex().to_string(),
                    json_pointer: Some("/fact".into()),
                    maximum_bytes: 128,
                },
            ),
        ]),
        permitted_attachments: BTreeMap::new(),
    };
    (
        template,
        BTreeMap::from([
            ("message.txt".into(), b"Hello Alex: Verified facts".to_vec()),
            (
                "slots.json".into(),
                br#"{"slots":{"name":"Alex","fact":"Verified facts"}}"#.to_vec(),
            ),
        ]),
        BTreeMap::from([("owner-facts".into(), source)]),
    )
}

#[test]
fn relentless_content_requires_exact_rendering_source_bytes_and_complete_file_binding() {
    let (template, files, sources) = sample();
    let proof = template.validate_artifact(&files, &sources).unwrap();
    let hashes: BTreeMap<_, _> = files
        .iter()
        .map(|(name, bytes)| (name.clone(), blake3::hash(bytes).to_hex().to_string()))
        .collect();
    template.validate_proof(&proof, &hashes).unwrap();
    for change in [
        "payload",
        "duplicate_slots",
        "choice",
        "source",
        "attachment",
    ] {
        let mut altered = files.clone();
        let mut changed_sources = sources.clone();
        match change {
            "payload" => {
                altered.insert("message.txt".into(), b"Hello Alex: Invented facts".to_vec());
            },
            "duplicate_slots" => {
                altered.insert(
                    "slots.json".into(),
                    br#"{"slots":{"name":"Alex","name":"Alex","fact":"Verified facts"}}"#.to_vec(),
                );
            },
            "choice" => {
                altered.insert(
                    "slots.json".into(),
                    br#"{"slots":{"name":"Someone else","fact":"Verified facts"}}"#.to_vec(),
                );
            },
            "source" => {
                changed_sources.insert(
                    "owner-facts".into(),
                    br#"{"fact":"Invented facts"}"#.to_vec(),
                );
            },
            _ => {
                altered.insert("unapproved.txt".into(), b"Extra content".to_vec());
            },
        }
        assert!(
            template
                .validate_artifact(&altered, &changed_sources)
                .is_err(),
            "{change}"
        );
    }
    let mut changed_hashes = hashes;
    changed_hashes.insert("message.txt".into(), "a".repeat(64));
    assert!(template.validate_proof(&proof, &changed_hashes).is_err());
}
