//! Destinations, fan-out policy and quiet hours (plan §6.1).
//!
//! Registration is eligibility, not proof of the recipient: a channel
//! destination is an owner identity the owner configured for that channel
//! type (`envoy.owner_identities` / `owner_identity_envs`) — the same
//! authority `chat::envoy::is_owner` uses — and never the last inbound
//! sender, an envoy contact, a display name, group membership or a
//! model-provided id. Group channels are outside this contract.
use std::collections::HashMap;

use chrono::{DateTime, NaiveTime, TimeZone, Utc};

use crate::config::{
    CriticalDeliveryPolicy, HitlCriticalDeliverySettings, MagicianConfig, QuietHoursSettings,
};

/// The live settings plus the owner identities they resolve to.
#[derive(Debug, Clone, Default)]
pub struct DeliveryPolicy {
    pub settings: HitlCriticalDeliverySettings,
    /// channel type → verified owner addresses, in configured order.
    pub owner_identities: HashMap<String, Vec<String>>,
    /// The public origin the secure link is built on, when configured.
    pub public_origin: Option<String>,
    /// How long a critical `otp` ask with automatic retrieval in progress
    /// waits before its channel alerts go out (§6.2; `0` = no grace).
    pub retrieval_grace_secs: u64,
}

/// One channel destination, in preference order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelDestination {
    pub channel_type: String,
    pub address: String,
}

impl DeliveryPolicy {
    pub fn from_config(config: &MagicianConfig) -> Self {
        let settings = config.hitl.critical_delivery.clone();
        let owner_identities = settings
            .enabled_channels
            .iter()
            .map(|channel| {
                (
                    channel.trim().to_ascii_lowercase(),
                    config.envoy.owner_identities_for(channel.trim()),
                )
            })
            .collect();
        // The owner's link first, the mobile origin only as the single-origin
        // default: where the UI and the device endpoint are different
        // hostnames, the mobile origin builds a link to a page that host does
        // not serve.
        let public_origin = settings.owner_link_origin(&config.mobile_access);
        Self {
            settings,
            owner_identities,
            public_origin,
            retrieval_grace_secs: if config.hitl.verification_codes.enabled {
                config.hitl.verification_codes.retrieval_grace_secs
            } else {
                0
            },
        }
    }

    /// The channel destinations, in the owner's preference order. A channel
    /// without an owner identity contributes nothing (and is reported as
    /// such by the settings status, not silently).
    pub fn channel_destinations(&self) -> Vec<ChannelDestination> {
        let mut out = Vec::new();
        for channel in &self.settings.enabled_channels {
            let channel_type = channel.trim().to_ascii_lowercase();
            if channel_type.is_empty()
                || out
                    .iter()
                    .any(|d: &ChannelDestination| d.channel_type == channel_type)
            {
                continue;
            }
            for address in self
                .owner_identities
                .get(&channel_type)
                .into_iter()
                .flatten()
            {
                out.push(ChannelDestination {
                    channel_type: channel_type.clone(),
                    address: address.clone(),
                });
            }
        }
        out
    }

    /// Enabled channels that have no owner identity configured — the honest
    /// status the settings surface shows.
    pub fn channels_without_owner(&self) -> Vec<String> {
        self.settings
            .enabled_channels
            .iter()
            .map(|c| c.trim().to_ascii_lowercase())
            .filter(|c| !c.is_empty())
            .filter(|c| self.owner_identities.get(c).map_or(true, Vec::is_empty))
            .collect()
    }

    pub fn staged(&self) -> bool {
        self.settings.policy == CriticalDeliveryPolicy::Staged
    }

    /// Whether an alert is held right now: inside the quiet window, unless
    /// the request is time-bound and the owner allows those through. The
    /// owner's own test is never held (the caller passes `time_bound = true`).
    pub fn quiet_hours_hold(&self, now: DateTime<Utc>, time_bound: bool) -> bool {
        let Some(quiet) = &self.settings.quiet_hours else {
            return false;
        };
        if time_bound && quiet.interrupt_for_time_bound {
            return false;
        }
        quiet_window_holds(quiet, now).is_some()
    }

    /// When the quiet window ends for an alert held now, in Unix millis.
    pub fn quiet_hours_end_ms(&self, now: DateTime<Utc>) -> Option<i64> {
        let quiet = self.settings.quiet_hours.as_ref()?;
        quiet_window_holds(quiet, now).map(|end| end.timestamp_millis())
    }
}

/// `Some(end)` when `now` is inside the quiet window, with the instant it
/// ends; `None` outside it or when the window is unparseable (an unparseable
/// window holds nothing — a misconfiguration must never silence alerts).
fn quiet_window_holds(quiet: &QuietHoursSettings, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let start = parse_hhmm(&quiet.start)?;
    let end = parse_hhmm(&quiet.end)?;
    if start == end {
        return None;
    }
    let tz: chrono_tz::Tz = quiet.timezone.trim().parse().unwrap_or(chrono_tz::UTC);
    let local = now.with_timezone(&tz);
    let today = local.date_naive();
    let at = |date: chrono::NaiveDate, time: NaiveTime| {
        tz.from_local_datetime(&date.and_time(time)).earliest()
    };
    let now_time = local.time();
    let (inside, end_local) = if start < end {
        // A window inside one day: 13:00–14:00.
        (now_time >= start && now_time < end, at(today, end))
    } else if now_time >= start {
        // Crossing midnight, the evening half: 22:00–07:00 at 23:00 ends
        // tomorrow at 07:00.
        (true, at(today.succ_opt()?, end))
    } else {
        // The morning half: 22:00–07:00 at 05:00 ends today at 07:00.
        (now_time < end, at(today, end))
    };
    if !inside {
        return None;
    }
    end_local.map(|end| end.with_timezone(&Utc))
}

fn parse_hhmm(text: &str) -> Option<NaiveTime> {
    let (hours, minutes) = text.trim().split_once(':')?;
    let hours: u32 = hours.parse().ok()?;
    let minutes: u32 = minutes.parse().ok()?;
    NaiveTime::from_hms_opt(hours, minutes, 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::EnvoyConfig;

    fn config_with(
        settings: HitlCriticalDeliverySettings,
        identities: &[(&str, &[&str])],
    ) -> MagicianConfig {
        let mut config = MagicianConfig::default();
        config.hitl.critical_delivery = settings;
        let mut envoy = EnvoyConfig::default();
        for (channel, addresses) in identities {
            envoy.owner_identities.insert(
                (*channel).to_string(),
                addresses.iter().map(|a| a.to_string()).collect(),
            );
        }
        config.envoy = envoy;
        config
    }

    #[test]
    fn destinations_are_owner_identities_of_enabled_channels_in_preference_order() {
        let settings = HitlCriticalDeliverySettings {
            enabled_channels: vec![
                "Telegram".into(),
                "kapso".into(),
                "telegram".into(),
                "slack".into(),
            ],
            ..Default::default()
        };
        let policy = DeliveryPolicy::from_config(&config_with(
            settings,
            &[
                ("kapso", &["919999900000"]),
                ("telegram", &["12345", "67890"]),
                ("whatsapp", &["1"]),
            ],
        ));
        let destinations = policy.channel_destinations();
        assert_eq!(
            destinations,
            vec![
                ChannelDestination {
                    channel_type: "telegram".into(),
                    address: "12345".into()
                },
                ChannelDestination {
                    channel_type: "telegram".into(),
                    address: "67890".into()
                },
                ChannelDestination {
                    channel_type: "kapso".into(),
                    address: "919999900000".into()
                },
            ],
            "whatsapp is registered but not enabled; slack is enabled but has no owner"
        );
        assert_eq!(policy.channels_without_owner(), vec!["slack".to_string()]);
        assert!(!policy.staged());
    }

    #[test]
    fn the_owner_link_origin_overrides_the_mobile_one_and_falls_back_to_it() {
        // One origin serving both is the ordinary case, and the mobile origin
        // is right there. Split them — the device hostname routed to the API,
        // the UI served elsewhere — and that origin builds a link to a page
        // the API does not serve, which a browser offers as a download.
        let mut config = config_with(HitlCriticalDeliverySettings::default(), &[]);
        config.mobile_access.public_origin = Some("https://connect.example.ai".into());
        assert_eq!(
            DeliveryPolicy::from_config(&config)
                .public_origin
                .as_deref(),
            Some("https://connect.example.ai"),
            "unset, the owner's link is built on the mobile origin",
        );

        config.hitl.critical_delivery.owner_ui_origin = Some("  https://ui.example.ai  ".into());
        assert_eq!(
            DeliveryPolicy::from_config(&config)
                .public_origin
                .as_deref(),
            Some("https://ui.example.ai"),
            "set, it wins and is trimmed",
        );

        config.hitl.critical_delivery.owner_ui_origin = Some("   ".into());
        assert_eq!(
            DeliveryPolicy::from_config(&config)
                .public_origin
                .as_deref(),
            Some("https://connect.example.ai"),
            "blank is not a configured origin",
        );
    }

    #[test]
    fn quiet_hours_hold_across_midnight_and_time_bound_requests_may_interrupt() {
        let quiet = QuietHoursSettings {
            start: "22:00".into(),
            end: "07:00".into(),
            timezone: "Asia/Kolkata".into(),
            interrupt_for_time_bound: true,
        };
        let settings = HitlCriticalDeliverySettings {
            quiet_hours: Some(quiet.clone()),
            ..Default::default()
        };
        let policy = DeliveryPolicy::from_config(&config_with(settings, &[]));
        // 23:30 IST = 18:00 UTC.
        let evening = Utc.with_ymd_and_hms(2026, 9, 22, 18, 0, 0).unwrap();
        assert!(policy.quiet_hours_hold(evening, false));
        assert!(
            !policy.quiet_hours_hold(evening, true),
            "a time-bound request interrupts"
        );
        // Ends tomorrow 07:00 IST = 01:30 UTC on the 23rd.
        assert_eq!(
            policy.quiet_hours_end_ms(evening),
            Some(
                Utc.with_ymd_and_hms(2026, 9, 23, 1, 30, 0)
                    .unwrap()
                    .timestamp_millis()
            )
        );
        // 05:00 IST = 23:30 UTC the day before: still inside, ends today 07:00 IST.
        let morning = Utc.with_ymd_and_hms(2026, 9, 22, 23, 30, 0).unwrap();
        assert!(policy.quiet_hours_hold(morning, false));
        assert_eq!(
            policy.quiet_hours_end_ms(morning),
            Some(
                Utc.with_ymd_and_hms(2026, 9, 23, 1, 30, 0)
                    .unwrap()
                    .timestamp_millis()
            )
        );
        // 12:00 IST = 06:30 UTC: outside.
        let noon = Utc.with_ymd_and_hms(2026, 9, 22, 6, 30, 0).unwrap();
        assert!(!policy.quiet_hours_hold(noon, false));
        // The owner can refuse interruptions.
        let strict = HitlCriticalDeliverySettings {
            quiet_hours: Some(QuietHoursSettings {
                interrupt_for_time_bound: false,
                ..quiet
            }),
            ..Default::default()
        };
        assert!(
            DeliveryPolicy::from_config(&config_with(strict, &[])).quiet_hours_hold(evening, true)
        );
        // An unparseable window holds nothing: misconfiguration never silences alerts.
        let broken = HitlCriticalDeliverySettings {
            quiet_hours: Some(QuietHoursSettings {
                start: "25:99".into(),
                end: "x".into(),
                timezone: "Mars/Olympus".into(),
                interrupt_for_time_bound: false,
            }),
            ..Default::default()
        };
        assert!(!DeliveryPolicy::from_config(&config_with(broken, &[]))
            .quiet_hours_hold(evening, false));
    }
}
