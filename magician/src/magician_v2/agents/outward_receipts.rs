//! Which provider message a send BECAME, taken from the sending tool's own
//! result.
//!
//! OPC tier 4. `docs/components/magician/delivery.md` is the gate this feeds.
//!
//! # The hole this closes
//!
//! Every live outward dispatch was marked `dispatch_unknown` the instant it
//! left, with a hardcoded reason, and the tool's result was discarded. Two
//! shipped senders hand back the provider's own message id in that result, so
//! the id existed and was thrown away — and without it a bounce, a complaint or
//! a delivery event has no join back to the disclosure it is about.
//! [`crate::magician_v2::delivery::DeliveryLedger::reconcile`] takes an act ref
//! it cannot discover for itself, which is why it has never had a caller.
//!
//! # Provider-agnostic by construction
//!
//! Nothing here speaks to a provider, holds a credential or knows an API. It
//! reads a result a capability already produced and names two strings: who
//! carried the message, and what they called it. A future SMTP adapter, a
//! Kapso, and an operator typing an id in by hand all land through the same
//! door — [`crate::magician_v2::evidence::OutwardAssertionStore::record_provider_message`]
//! — because a receipt is a receipt.
//!
//! What is unavoidably per-capability is the SHAPE: `agentmail-send` says
//! `message_id` at the top level and `kapso-whatsapp-send` says
//! `messages[0].id`, and no amount of genericity makes those one shape. So the
//! shapes live in one small table keyed by capability, honestly small, and
//! everything not in it is a stated gap rather than a guess.
//!
//! # Fail closed, and the distinction that is the whole point
//!
//! A result with no recoverable id leaves the act at `dispatch_unknown` with a
//! reason saying which capability and where the id was looked for. That is
//! **not** a failure to send. It is a failure to KNOW — the act may well have
//! arrived, and the state says exactly that rather than guessing either way.
//! An absence is never promoted into a success anywhere in this module: the
//! only value that can move an act forward is an id that was actually read out
//! of the result and survived validation.

use serde_json::Value;

/// Above this, a send response is not a send response.
///
/// A `{"message_id": …}` is tens of bytes. A megabyte of stdout arriving here
/// is a capability streaming something at us, and parsing it to look for an id
/// spends real time on the dispatch path for an answer that is not there. Over
/// budget is a stated gap, never a silent skip.
const MAX_RESULT_BYTES: usize = 65_536;

/// Where a provider's message id sits in its send response.
///
/// Three shapes, because three shapes is what the shipped senders actually use —
/// no fourth is declared for a channel that might one day need it, because a
/// variant nothing constructs is a design nobody has tested. A new provider
/// arrives as a new variant, not as a path expression language: a mini-language
/// here would be a thing nobody can test exhaustively, guarding the id that
/// decides whether a complaint reaches the right disclosure.
///
/// [`Self::Pair`] arrived with the Telegram channels and is the closest this
/// gets to a path, deliberately bounded: two fixed key sequences and a join,
/// with no array index, no wildcard and no branching. It exists because one
/// provider's id is genuinely not one field, not because a general reader
/// seemed convenient.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReceiptShape {
    /// `{"message_id": "…"}` — a top-level scalar field.
    Top(&'static str),
    /// `{"messages": [{"id": "…"}]}` — a named field on the FIRST element of a
    /// top-level array.
    FirstIn {
        array: &'static str,
        field: &'static str,
    },
    /// Two fields joined, because the id one of them holds is not an identifier
    /// on its own.
    ///
    /// Telegram numbers messages **per chat**: the first message sent into every
    /// chat is `message_id: 1`. A bare `message_id` would therefore file two
    /// different messages under one key, and the damage is not a duplicate row.
    /// [`crate::magician_v2::evidence::OutwardAssertionStore::record_provider_message`]
    /// refuses the second claim outright — so the second send never reconciles —
    /// and a delivery event carrying that id resolves to whichever act claimed it
    /// FIRST. That is a complaint landing on a disclosure that never carried it,
    /// which is the one outcome this whole tier exists to prevent.
    ///
    /// Each half is a fixed sequence of object keys read left to right. No array
    /// index, no wildcard, no expression: two lookups and a join, which is the
    /// smallest thing that turns a per-chat number into an id. Both halves are
    /// required — a pair missing its scope would fall back to exactly the
    /// non-identifier this variant exists to refuse.
    Pair {
        scope: &'static [&'static str],
        id: &'static [&'static str],
    },
}

/// Joins the two halves of a [`ReceiptShape::Pair`].
///
/// `/` because that is how Telegram itself renders a message reference
/// (`t.me/c/<chat>/<message_id>`), and because the store refuses only control
/// characters — a separator that could tear an index line would be a receipt
/// that never finds its disclosure.
const PAIR_SEP: char = '/';

impl ReceiptShape {
    /// A human description of where the id was looked for, so a gap can say so.
    fn describe(self) -> String {
        match self {
            Self::Top(field) => field.to_string(),
            Self::FirstIn { array, field } => format!("{array}[0].{field}"),
            Self::Pair { scope, id } => format!("{} + {}", scope.join("."), id.join(".")),
        }
    }

    /// The id this shape names, already reduced to a string.
    ///
    /// Returns an owned value rather than a borrowed [`Value`] because a
    /// [`Self::Pair`] has no single node to borrow — its id is two nodes joined.
    /// A single-node shape hands back whatever the provider wrote, blank
    /// included, so the blank check below can report it as `IdUnusable` rather
    /// than as an absence. A pair cannot: a blank half must not be joined.
    fn read_id(self, root: &Value) -> Option<String> {
        match self {
            Self::Top(field) => scalar_id(root.get(field)?),
            Self::FirstIn { array, field } => {
                scalar_id(root.get(array)?.as_array()?.first()?.get(field)?)
            },
            Self::Pair { scope, id } => {
                let scope = non_blank(scalar_id(walk(root, scope)?)?)?;
                let id = non_blank(scalar_id(walk(root, id)?)?)?;
                Some(format!("{scope}{sep}{id}", sep = PAIR_SEP))
            },
        }
    }
}

/// Follow a fixed sequence of object keys. Nothing else — no array index, no
/// wildcard, and an absent key is an absence rather than an empty object.
fn walk<'a>(root: &'a Value, path: &[&str]) -> Option<&'a Value> {
    path.iter().try_fold(root, |node, key| node.get(key))
}

/// A trimmed value, or `None` when it was only whitespace.
///
/// Applied to each half of a pair separately, not to the joined string: a blank
/// scope with a real id would otherwise survive as `/4241`, which is the bare
/// per-chat number wearing a separator.
fn non_blank(value: String) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// One capability's send-response shape.
struct ReceiptSource {
    capability: &'static str,
    /// Action tokens whose result IS a send response. Empty means every action
    /// on this capability is a send — the send-only capabilities, where the
    /// bare invocation is the act.
    ///
    /// Named rather than inferred from "this dispatch was classified outward",
    /// and that is load-bearing: a capability is also classified outward when
    /// it merely carries an argv escape hatch, so a `triage` call with
    /// `extra_args` reaches the gate. Reading an id out of a triage result
    /// would bind a mailbox READ's id onto a disclosure and report a message
    /// that was never sent as provider-accepted.
    sending_actions: &'static [&'static str],
    /// The name this provider is known by in a receipt. Not the capability:
    /// `presto-gmail` and `gmail` are two capabilities addressing one provider,
    /// and a bounce from either is a bounce from Gmail.
    provider: &'static str,
    shape: ReceiptShape,
}

/// What each shipped sender's own result says, verified against the pinned
/// client each one runs.
///
/// - **`agentmail-send`** → `agentmail-cli`, whose SDK types the workspace pins
///   declare `SendMessageResponse { message_id, thread_id }`. Verified from the
///   shipped package, not from documentation.
/// - **`kapso-whatsapp-send`** → the Kapso CLI's `whatsapp messages send`,
///   which prints its client's `SendMessageResponse { messagingProduct,
///   contacts, messages: [{ id, messageStatus }] }` with the keys
///   decamelised, so the id is `messages[0].id` — a Meta `wamid`. Verified from
///   the shipped CLI command and its typed client.
/// - **`gmail` / `presto-gmail`** → `gws gmail +send`, which posts to
///   `users.messages.send` and prints the API's own JSON. The Gmail API returns
///   a `Message` resource, whose id field is `id`. This one is **inferred from
///   the API contract**: confirming it needs a live OAuth profile and a real
///   send, which is not something a build may do. It is safe to carry
///   unconfirmed because the read is shape-strict — if `gws` wraps its output
///   differently, no `id` is found and the act stays exactly where it is today.
/// - **`whatsapp`** → `wu messages send <jid> <text> --json`. The pinned
///   `@ibrahimwithi/wu-cli@0.1.20` prints, verbatim from `dist/cli/messages.js`,
///   an object holding the Baileys message key's `id` and the message
///   timestamp — so the id is a top-level `id`. Verified from the shipped
///   package, not from the skill text. **`--json` is load-bearing**: without it
///   the same command prints a human line, which parses as nothing and leaves
///   the act unreconcilable. The shipped `send` action appends it through a
///   literal mapping, which is why this entry can exist at all.
/// - **`telegram`** → the bundled adapter's `send` route, which fixes the
///   method to `sendMessage` and writes its own receipt:
///   `{"ok":true,"method":…,"chat_id":…,"message_id":…,"result":{…}}`. Verified
///   from `skillshub/telegram/bin/telegram-bot-adapter`, not from the skill
///   text. The `run` route still echoes the API body verbatim, which is one
///   more reason this entry names `send` alone. The id is a PAIR — see
///   [`ReceiptShape::Pair`] — because the lift made `message_id` *reachable*
///   without making it *unique*: Telegram numbers messages per chat.
/// - **`telegram-self`** → the `send` action routes `send text --to … --message
///   … --json`, and `runSendText` in the pinned `@dapi/tgcli@2.4.0` writes
///   `{ channelId: <the target as the caller spelled it>, messageId: <id> }`.
///   Verified from the shipped package; the skill's own description of the
///   result agrees. A pair for the same reason, with the honest weakness that
///   its scope half is the caller's spelling of the chat rather than a
///   canonical id — `@alice`, `+1555…` and a numeric id all reach one person
///   and give three different keys. The binding direction therefore works and
///   the *lookup* direction (an inbound event, which knows the canonical chat)
///   can miss. A miss leaves an act unreconciled, which is the fail-closed side,
///   and `telegram-bot` does not share it because its adapter canonicalises
///   `chat_id` before sending.
///
/// The three chat entries are keyed on **`send` and nothing else**, which is
/// the same discipline `gmail` needs and for a sharper reason: each of these
/// capabilities also exposes a free-text `run` whose result may be a chat list,
/// a contact search or a status probe. `whatsapp run command="status --json"`
/// returns an object; reading an id out of it would bind a *readiness check* to
/// a disclosure as the message a send became. The `telegram` adapter makes the
/// point in its own code — the `send` route writes a receipt, the `run` route
/// echoes the provider body verbatim, and only the first is described here.
///
/// Everything else outward is absent on purpose, and absent means **that
/// channel can never be reconciled until its skill changes**. See
/// `docs/components/magician/delivery.md` for the per-channel list and what
/// each one would have to start returning.
const RECEIPT_SOURCES: &[ReceiptSource] = &[
    ReceiptSource {
        capability: "agentmail-send",
        sending_actions: &[],
        provider: "agentmail",
        shape: ReceiptShape::Top("message_id"),
    },
    ReceiptSource {
        capability: "kapso-whatsapp-send",
        sending_actions: &[],
        provider: "kapso",
        shape: ReceiptShape::FirstIn {
            array: "messages",
            field: "id",
        },
    },
    ReceiptSource {
        capability: "gmail",
        sending_actions: &["send", "reply", "reply_all", "forward"],
        provider: "gmail",
        shape: ReceiptShape::Top("id"),
    },
    ReceiptSource {
        capability: "presto-gmail",
        sending_actions: &["send", "reply", "reply_all", "forward"],
        provider: "gmail",
        shape: ReceiptShape::Top("id"),
    },
    ReceiptSource {
        capability: "whatsapp",
        sending_actions: &["send"],
        // NOT `kapso`, though both carry WhatsApp. These are two transports
        // with two id namespaces — a Baileys message key from the owner's own
        // WhatsApp Web session here, a Meta `wamid` through Kapso's Cloud API
        // there — and one provider name over both would let an event about one
        // resolve to the other's disclosure.
        provider: "whatsapp",
        shape: ReceiptShape::Top("id"),
    },
    ReceiptSource {
        capability: "telegram",
        sending_actions: &["send"],
        // Named apart from `telegram-self` because a bot's numeric `chat.id`
        // and a caller-spelled `@username` are not the same namespace, and
        // `record_provider_message` refuses a second act claiming one key —
        // so a collision across the two would leave a real send permanently
        // unreconciled while the first act absorbed its delivery events.
        provider: "telegram-bot",
        // Still a PAIR even though the adapter lifted the id to the top level.
        // The lift solved reachability; it did not make `message_id` unique.
        // `chat_id` is the value the adapter actually POSTed — canonicalised by
        // it, int or `@username`, refused if neither — so the scope half is the
        // recipient the gate screened rather than a spelling of it.
        shape: ReceiptShape::Pair {
            scope: &["chat_id"],
            id: &["message_id"],
        },
    },
    ReceiptSource {
        capability: "telegram-self",
        sending_actions: &["send"],
        provider: "telegram-user",
        shape: ReceiptShape::Pair {
            scope: &["channelId"],
            id: &["messageId"],
        },
    },
];

/// Who carried a message, and what they called it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderMessage {
    pub provider: String,
    pub provider_message_id: String,
}

/// Why no provider message could be taken from a result.
///
/// Every variant is a reason an act must stay `dispatch_unknown`, and each
/// carries enough to say which capability and where the id was looked for. A
/// single opaque "no id" would produce the state this tier exists to end: a
/// permanent unknown nobody can act on because nothing says what is missing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReceiptGap {
    /// This capability and action are not a send this table describes as one.
    /// The common case, and the quiet one: it covers every non-outward dispatch
    /// as well as an outward capability invoked on a reading action.
    NotADescribedSend { capability: String },
    /// The result carried no text to read at all.
    NoResultText { capability: String },
    /// Too much text to be a send response.
    ResultTooLarge { capability: String, bytes: usize },
    /// Text that is not a JSON object.
    ResultNotJson { capability: String },
    /// A JSON object, with nothing at the place this capability's id lives.
    IdAbsent {
        capability: String,
        looked_at: String,
    },
    /// An id was there and cannot be used — blank, or carrying a control
    /// character that would tear the index line it has to be filed under.
    IdUnusable { capability: String, why: String },
}

impl ReceiptGap {
    /// The detail written onto the act's `dispatch_unknown` transition.
    ///
    /// Phrased as a failure to KNOW, never as a failure to send. The act may
    /// well have arrived; what is missing is any way to ask.
    pub fn reason(&self) -> String {
        match self {
            Self::NotADescribedSend { capability } => format!(
                "`{capability}` has no described send-response shape: this send may have \
                 arrived and nothing can be asked about it"
            ),
            Self::NoResultText { capability } => format!(
                "`{capability}` returned no readable text, so no provider message id could be \
                 taken from it: unreconcilable, which is not the same as unsent"
            ),
            Self::ResultTooLarge { capability, bytes } => format!(
                "`{capability}` returned {bytes} bytes, past the {MAX_RESULT_BYTES} a send \
                 response is read up to, so no provider message id was taken: unreconcilable, \
                 which is not the same as unsent"
            ),
            Self::ResultNotJson { capability } => format!(
                "`{capability}` returned text that is not a JSON object, so no provider message \
                 id could be taken from it: unreconcilable, which is not the same as unsent"
            ),
            Self::IdAbsent {
                capability,
                looked_at,
            } => format!(
                "`{capability}` returned JSON with nothing at `{looked_at}`, so this send cannot \
                 be reconciled: unreconcilable, which is not the same as unsent"
            ),
            Self::IdUnusable { capability, why } => format!(
                "`{capability}` returned a provider message id that cannot be used ({why}), so \
                 this send cannot be reconciled: unreconcilable, which is not the same as unsent"
            ),
        }
    }
}

/// Take the provider message out of one outward tool's result.
///
/// `capability` and `action` are the coordinates the outward gate already
/// resolved for this dispatch, and `result_text` is the tool's own output.
///
/// **The only path by which an act can advance past `dispatch_unknown`.** Every
/// arm that is not a validated id returns a [`ReceiptGap`], so there is no
/// branch here in which an absence becomes a positive.
pub fn provider_message_from_result(
    capability: &str,
    action: &str,
    result_text: Option<&str>,
) -> Result<ProviderMessage, ReceiptGap> {
    // A compiled leaf arrives as `pack__action` — `whatsapp__send` is what
    // `ExecutableAction::Pack` carries and what `outward_settle` hands over
    // verbatim. Matching that string against the table finds nothing, so every
    // leaf-shaped send would report `NotADescribedSend` and stay unreconciled
    // while its id sat in the result. Resolved through the same function
    // `outward_actions::capability_key` uses, so the classifier and this table
    // cannot disagree about which capability a dispatch names.
    let key = super::approval::pack_tool_for_approval(capability.trim())
        .trim()
        .to_ascii_lowercase();
    let action_key = action.trim().to_ascii_lowercase();

    let Some(source) = RECEIPT_SOURCES.iter().find(|source| {
        source.capability == key
            && (source.sending_actions.is_empty()
                || source.sending_actions.contains(&action_key.as_str()))
    }) else {
        return Err(ReceiptGap::NotADescribedSend { capability: key });
    };

    let Some(text) = result_text.map(str::trim).filter(|text| !text.is_empty()) else {
        return Err(ReceiptGap::NoResultText { capability: key });
    };
    if text.len() > MAX_RESULT_BYTES {
        return Err(ReceiptGap::ResultTooLarge {
            capability: key,
            bytes: text.len(),
        });
    }
    let Ok(root) = serde_json::from_str::<Value>(text) else {
        return Err(ReceiptGap::ResultNotJson { capability: key });
    };
    if !root.is_object() {
        return Err(ReceiptGap::ResultNotJson { capability: key });
    }

    // `read_id` already applies `scalar_id` at each shape's leaf and returns
    // the string; chaining another `scalar_id` here would be a second pass over
    // a value that is no longer a `Value`.
    let Some(raw) = source.shape.read_id(&root) else {
        return Err(ReceiptGap::IdAbsent {
            capability: key,
            looked_at: source.shape.describe(),
        });
    };

    let id = raw.trim();
    if id.is_empty() {
        return Err(ReceiptGap::IdUnusable {
            capability: key,
            why: "blank".to_string(),
        });
    }
    // The id becomes part of an index key on an append-only file. A control
    // character in it — U+001F above all, the separator between provider and
    // id — would shift a component boundary or tear the line, and a torn
    // pointer is a receipt that can never find its disclosure. The store
    // refuses these too; refusing here as well means the gap is REPORTED as a
    // gap rather than surfacing as an opaque write error.
    if id.chars().any(char::is_control) {
        return Err(ReceiptGap::IdUnusable {
            capability: key,
            why: "carries a control character".to_string(),
        });
    }

    Ok(ProviderMessage {
        provider: source.provider.to_string(),
        provider_message_id: id.to_string(),
    })
}

/// A provider id as a string, whatever JSON scalar the provider chose.
///
/// Strings and integers only. A float id would be a parsed-and-reformatted
/// number that no longer matches what the provider sent, and a bool or a null
/// is not an id — all three are absences, and an absence must stay one.
fn scalar_id(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => number
            .as_u64()
            .map(|n| n.to_string())
            .or_else(|| number.as_i64().map(|n| n.to_string())),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The id the runtime threw away for the entire life of the feature.
    ///
    /// Pinned against the shape `agentmail-cli`'s own `SendMessageResponse`
    /// declares, so the test fails if the table is edited to look somewhere
    /// else.
    #[test]
    fn an_agentmail_send_names_its_message() {
        let taken = provider_message_from_result(
            "agentmail-send",
            "send",
            Some(r#"{"message_id":"msg_01H8","thread_id":"thr_77"}"#),
        )
        .expect("agentmail names message_id");
        assert_eq!(taken.provider, "agentmail");
        assert_eq!(taken.provider_message_id, "msg_01H8");
    }

    /// A WhatsApp send's id is one level down, inside the array Meta returns.
    #[test]
    fn a_kapso_send_names_the_wamid_inside_its_messages_array() {
        let taken = provider_message_from_result(
            "kapso-whatsapp-send",
            "send",
            Some(
                r#"{"messaging_product":"whatsapp",
                    "contacts":[{"input":"+15551234567","wa_id":"15551234567"}],
                    "messages":[{"id":"wamid.HBgLMTU1NTE","message_status":"accepted"}]}"#,
            ),
        )
        .expect("kapso names messages[0].id");
        assert_eq!(taken.provider, "kapso");
        assert_eq!(taken.provider_message_id, "wamid.HBgLMTU1NTE");
    }

    /// Two capabilities, one provider. A bounce from Presto's Gmail and from
    /// the owner's is a bounce from Gmail, and a receipt that named the
    /// capability would file the two under different providers.
    #[test]
    fn both_gmail_capabilities_report_one_provider() {
        for capability in ["gmail", "presto-gmail"] {
            let taken = provider_message_from_result(
                capability,
                "send",
                Some(r#"{"id":"18f3a2b1c4d5e6f7","threadId":"18f3a2b1c4d5e600"}"#),
            )
            .expect("gws prints the Gmail Message resource");
            assert_eq!(taken.provider, "gmail");
            assert_eq!(taken.provider_message_id, "18f3a2b1c4d5e6f7");
        }
    }

    /// The id `wu` prints and nothing was reading.
    ///
    /// Pinned against the shape the pinned `@ibrahimwithi/wu-cli@0.1.20`
    /// actually prints under `--json` — the Baileys message key id and the
    /// message timestamp — read out of `dist/cli/messages.js`, not out of the
    /// skill's prose.
    #[test]
    fn a_whatsapp_send_names_its_message() {
        let taken = provider_message_from_result(
            "whatsapp",
            "send",
            Some(r#"{"id":"3EB0C767D82B0F3A1B2C","timestamp":1755500000}"#),
        )
        .expect("wu prints the message key id");
        assert_eq!(
            taken.provider, "whatsapp",
            "NOT `kapso`: a WhatsApp Web key and a Meta wamid are two id namespaces, and one \
             provider name over both would let an event about one resolve to the other's act"
        );
        assert_eq!(taken.provider_message_id, "3EB0C767D82B0F3A1B2C");
    }

    /// A Telegram send is identified by its CHAT and its number, never by the
    /// number alone.
    ///
    /// Telegram counts messages per chat, so the first message into every chat
    /// is `message_id: 1`. Two sends into two chats therefore hand back the
    /// same number, and a bare `message_id` would file them under one key —
    /// which `record_provider_message` refuses outright, leaving the second
    /// send permanently unreconciled while the first act absorbed any delivery
    /// event carrying that number.
    #[test]
    fn a_telegram_send_is_identified_by_its_chat_and_its_number() {
        let first = provider_message_from_result(
            "telegram",
            "send",
            Some(
                r#"{"ok":true,"method":"sendMessage","chat_id":-1001234567890,
                    "message_id":1,"result":{"message_id":1,"text":"hi"}}"#,
            ),
        )
        .expect("the adapter lifts chat_id and message_id to the top level");
        assert_eq!(first.provider, "telegram-bot");
        assert_eq!(first.provider_message_id, "-1001234567890/1");

        // A different chat, the same per-chat number. Two acts, two keys.
        let second = provider_message_from_result(
            "telegram",
            "send",
            Some(r#"{"ok":true,"method":"sendMessage","chat_id":"@ana","message_id":1}"#),
        )
        .expect("a username chat is a chat");
        assert_eq!(second.provider_message_id, "@ana/1");
        assert_ne!(
            first.provider_message_id, second.provider_message_id,
            "two different messages sharing a per-chat number must not share a key"
        );
    }

    /// The operator's own account is a DIFFERENT provider from the bot.
    ///
    /// Its scope half is `channelId`, which the CLI echoes back exactly as the
    /// caller spelled the target — so the two namespaces cannot be compared,
    /// and filing both under `telegram` would let one act's key resolve the
    /// other's.
    #[test]
    fn a_telegram_self_send_is_its_own_provider() {
        let taken = provider_message_from_result(
            "telegram-self",
            "send",
            Some("{\n  \"channelId\": \"@ana\",\n  \"messageId\": 4241\n}"),
        )
        .expect("tgcli writes channelId and messageId");
        assert_eq!(taken.provider, "telegram-user");
        assert_eq!(taken.provider_message_id, "@ana/4241");
        assert_ne!(
            taken.provider, "telegram-bot",
            "the bot's outbox and the operator's are not one id namespace"
        );
    }

    /// Half a pair is not an id.
    ///
    /// The Telegram adapter leaves `message_id` ABSENT when the provider did not
    /// return an integer one, so this is the shape a refused-then-retried send
    /// can really produce. Joining what is there would yield a bare per-chat
    /// number or a bare chat — neither identifies a message — so both halves are
    /// required and the gap names where it looked.
    #[test]
    fn a_pair_missing_either_half_is_a_named_gap() {
        for body in [
            r#"{"ok":true,"method":"sendMessage","chat_id":-100123}"#,
            r#"{"ok":true,"method":"sendMessage","message_id":7}"#,
            r#"{"ok":true,"method":"sendMessage","chat_id":"  ","message_id":7}"#,
            r#"{"ok":true,"method":"sendMessage","chat_id":-100123,"message_id":null}"#,
        ] {
            let gap = provider_message_from_result("telegram", "send", Some(body))
                .expect_err("half a pair identifies nothing");
            assert_eq!(
                gap,
                ReceiptGap::IdAbsent {
                    capability: "telegram".to_string(),
                    looked_at: "chat_id + message_id".to_string(),
                },
                "body {body} must read as an absence"
            );
            assert!(
                gap.reason().contains("not the same as unsent"),
                "{}",
                gap.reason()
            );
        }
    }

    /// The residual, in this module's currency: a send composed inside `run`
    /// yields no receipt, because `run` is not a described send.
    ///
    /// This is the same keying that stops a `gmail triage` id being recorded,
    /// and here it is doing double duty. `whatsapp run command="status --json"`
    /// returns an object; the `telegram` adapter's `run` route echoes the Bot
    /// API body verbatim, `message_id` and all. Reading an id out of either
    /// would bind a readiness probe — or a message the gate never saw, screened
    /// nobody for and wrote no disclosure about — onto a disclosure as the
    /// message a send became.
    #[test]
    fn a_send_composed_inside_run_yields_no_receipt() {
        for (capability, body) in [
            (
                "whatsapp",
                r#"{"id":"3EB0C767D82B0F3A","timestamp":1755500000}"#,
            ),
            (
                "telegram",
                r#"{"ok":true,"result":{"message_id":9,"chat":{"id":-100123}}}"#,
            ),
            ("telegram-self", r#"{"channelId":"@ana","messageId":4241}"#),
        ] {
            let gap = provider_message_from_result(capability, "run", Some(body))
                .expect_err("`run` is not a described send, however send-like its result looks");
            assert_eq!(
                gap,
                ReceiptGap::NotADescribedSend {
                    capability: capability.to_string()
                }
            );
        }
        // A reaction is not a send either, and its result is not a message id.
        assert!(provider_message_from_result(
            "whatsapp",
            "react",
            Some(r#"{"id":"3EB0C767D82B0F3A"}"#)
        )
        .is_err());
    }

    /// A compiled leaf arrives as `pack__action`, and matching the raw name
    /// found nothing in this table.
    ///
    /// `ExecutableAction::Pack` carries `whatsapp__send`, and
    /// `outward_settle::settle_outward_dispatch` hands that string over
    /// verbatim — while the classifier that let the dispatch through DOES split
    /// it. So a leaf-shaped send was classified outward, written to a
    /// disclosure, and then reported `NotADescribedSend` with its id sitting in
    /// the result. The two must resolve a capability the same way or this tier
    /// is inert for exactly the calling shape the catalog offers.
    #[test]
    fn a_compiled_leaf_resolves_to_the_capability_it_names() {
        let taken = provider_message_from_result(
            "whatsapp__send",
            "send",
            Some(r#"{"id":"3EB0C767D82B0F3A","timestamp":1755500000}"#),
        )
        .expect("`whatsapp__send` is `whatsapp`");
        assert_eq!(taken.provider, "whatsapp");
        assert_eq!(taken.provider_message_id, "3EB0C767D82B0F3A");

        // The hyphenated capability, whose own name must not lose its tail to
        // the split.
        let taken = provider_message_from_result(
            "telegram-self__send",
            "send",
            Some(r#"{"channelId":"@ana","messageId":4241}"#),
        )
        .expect("`telegram-self__send` is `telegram-self`");
        assert_eq!(taken.provider, "telegram-user");
        assert_eq!(taken.provider_message_id, "@ana/4241");

        // And splitting must not invent a described send: the read leaf of a
        // described capability is still not one.
        assert_eq!(
            provider_message_from_result(
                "whatsapp__run",
                "run",
                Some(r#"{"id":"3EB0C767D82B0F3A"}"#)
            ),
            Err(ReceiptGap::NotADescribedSend {
                capability: "whatsapp".to_string()
            })
        );
    }

    /// A mailbox READ that reached the outward gate through an argv escape
    /// hatch must not have its id bound onto a disclosure.
    ///
    /// `gmail` is classified outward whenever it carries `extra_args`,
    /// whatever the action token says — so a `triage` result reaches this
    /// function, and Gmail resources carry a top-level `id`. Keying the table
    /// on the SENDING action tokens is what stops a read's id being recorded
    /// as the message a send became.
    #[test]
    fn a_read_on_a_mail_capability_yields_no_provider_message() {
        let gap = provider_message_from_result(
            "gmail",
            "triage",
            Some(r#"{"id":"18f3a2b1c4d5e6f7","snippet":"unread"}"#),
        )
        .expect_err("triage is not a send");
        assert_eq!(
            gap,
            ReceiptGap::NotADescribedSend {
                capability: "gmail".to_string()
            }
        );
    }

    /// A send whose result carries no id stays unknown, and says why.
    #[test]
    fn a_send_with_no_id_in_its_result_is_a_named_gap() {
        let gap = provider_message_from_result(
            "agentmail-send",
            "send",
            Some(r#"{"thread_id":"thr_77"}"#),
        )
        .expect_err("no message_id");
        assert_eq!(
            gap,
            ReceiptGap::IdAbsent {
                capability: "agentmail-send".to_string(),
                looked_at: "message_id".to_string(),
            }
        );
        let reason = gap.reason();
        assert!(
            reason.contains("`message_id`"),
            "the reason must name where the id was looked for: {reason}"
        );
        assert!(
            reason.contains("not the same as unsent"),
            "unreconcilable is a failure to KNOW, and the reason must say so: {reason}"
        );
    }

    /// An empty or unreadable result is never a success.
    #[test]
    fn an_empty_result_is_a_gap_and_not_an_id() {
        for text in [None, Some(""), Some("   ")] {
            let gap = provider_message_from_result("agentmail-send", "send", text)
                .expect_err("nothing to read");
            assert_eq!(
                gap,
                ReceiptGap::NoResultText {
                    capability: "agentmail-send".to_string()
                }
            );
        }
    }

    /// A CLI that printed a human line instead of JSON is a gap, not a crash
    /// and not a send that silently counted.
    #[test]
    fn a_non_json_result_is_a_gap() {
        let gap = provider_message_from_result("agentmail-send", "send", Some("Sent to alice."))
            .expect_err("not JSON");
        assert_eq!(
            gap,
            ReceiptGap::ResultNotJson {
                capability: "agentmail-send".to_string()
            }
        );
    }

    /// A JSON ARRAY is not a send response, and its first element's fields must
    /// not be read as one.
    #[test]
    fn a_json_array_result_is_not_a_send_response() {
        let gap =
            provider_message_from_result("agentmail-send", "send", Some(r#"[{"message_id":"x"}]"#))
                .expect_err("an array is not a send response");
        assert_eq!(
            gap,
            ReceiptGap::ResultNotJson {
                capability: "agentmail-send".to_string()
            }
        );
    }

    /// An id carrying the index separator is refused before it can be filed.
    ///
    /// U+001F is what joins the provider to the id in the index key, so an id
    /// holding one could shift the boundary and file this act under another
    /// provider's message — a bounce landing on a disclosure that never sent it.
    #[test]
    fn an_id_carrying_the_field_separator_is_refused() {
        // The separator arrives ESCAPED, which is the only way it can arrive:
        // JSON forbids a raw control character inside a string, so a provider
        // that wanted to smuggle one has to write `\\u001f` and let the parser
        // decode it — which is exactly what this reads back.
        let gap = provider_message_from_result(
            "agentmail-send",
            "send",
            Some(r#"{"message_id":"msg\u001fkapso"}"#),
        )
        .expect_err("U+001F is the separator");
        assert_eq!(
            gap,
            ReceiptGap::IdUnusable {
                capability: "agentmail-send".to_string(),
                why: "carries a control character".to_string(),
            }
        );
    }

    /// A blank id is an absence wearing a field name.
    #[test]
    fn a_blank_id_is_refused() {
        let gap =
            provider_message_from_result("agentmail-send", "send", Some(r#"{"message_id":"  "}"#))
                .expect_err("blank");
        assert_eq!(
            gap,
            ReceiptGap::IdUnusable {
                capability: "agentmail-send".to_string(),
                why: "blank".to_string(),
            }
        );
    }

    /// A bool or a null where the id should be is an absence, not an id
    /// spelled `true`.
    #[test]
    fn a_non_scalar_id_is_absent_rather_than_stringified() {
        for body in [
            r#"{"message_id":true}"#,
            r#"{"message_id":null}"#,
            r#"{"message_id":{"id":"nested"}}"#,
            r#"{"message_id":1.5}"#,
        ] {
            let gap = provider_message_from_result("agentmail-send", "send", Some(body))
                .expect_err("not a usable scalar");
            assert_eq!(
                gap,
                ReceiptGap::IdAbsent {
                    capability: "agentmail-send".to_string(),
                    looked_at: "message_id".to_string(),
                },
                "body {body} must read as an absence"
            );
        }
    }

    /// An integer id — the shape a Telegram-like provider would return — is
    /// carried as its exact decimal digits.
    #[test]
    fn an_integer_id_survives_as_its_digits() {
        assert_eq!(
            scalar_id(&serde_json::json!(4241)),
            Some("4241".to_string())
        );
        assert_eq!(scalar_id(&serde_json::json!(-7)), Some("-7".to_string()));
        assert_eq!(scalar_id(&serde_json::json!(1.5)), None);
    }

    /// The channels that can never be reconciled say so, rather than going
    /// quiet.
    ///
    /// Each of these is a real shipped outward capability whose send result
    /// carries no message id — `imessage_send` returns AppleScript's exit
    /// status, a calendar write returns an Event resource whose id is not a
    /// per-recipient message id, and a browser returns a page. Feeding each a
    /// result that DOES contain a plausible id pins that the table refuses to
    /// guess for them: an id read out of an undescribed channel would be a
    /// message id nobody verified, bound to a disclosure a bounce could then
    /// land on.
    ///
    /// `whatsapp`, `telegram` and `telegram-self` used to be in this list, for a
    /// different reason — their only action was a free-text `run` whose token
    /// could not say whether a dispatch was a send. They left it by NAMING the
    /// send, not by being guessed at; `a_send_composed_inside_run_yields_no_receipt`
    /// holds the half that did not change.
    #[test]
    fn an_undescribed_channel_refuses_to_guess_an_id() {
        for capability in ["imessage_send", "calendar", "presto-calendar", "browser"] {
            let gap = provider_message_from_result(
                capability,
                "send",
                Some(r#"{"id":"looks-like-an-id","message_id":"so-does-this"}"#),
            )
            .expect_err("no described shape means no id, however plausible the result looks");
            assert_eq!(
                gap,
                ReceiptGap::NotADescribedSend {
                    capability: capability.to_string()
                }
            );
            assert!(
                gap.reason().contains("may have arrived"),
                "an undescribed channel is unreconcilable, not unsent: {}",
                gap.reason()
            );
        }
    }

    /// A result far larger than any send response is refused before it is
    /// parsed, and the refusal is a stated gap.
    #[test]
    fn an_oversized_result_is_a_stated_gap() {
        let huge = format!(r#"{{"message_id":"{}"}}"#, "x".repeat(MAX_RESULT_BYTES));
        let gap = provider_message_from_result("agentmail-send", "send", Some(&huge))
            .expect_err("over budget");
        assert!(matches!(gap, ReceiptGap::ResultTooLarge { .. }));
        assert!(
            gap.reason().contains("65536"),
            "the reason must name the budget it exceeded: {}",
            gap.reason()
        );
    }
}
