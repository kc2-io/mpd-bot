//! Bounded, session-only diagnostics. Callers supply events and typed fields, never raw text.
use std::{
    collections::VecDeque,
    fmt,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

const MAX_ENTRIES: usize = 1_000;
const MAX_BYTES: usize = 2 * 1024 * 1024;

#[derive(Clone)]
pub struct SecretValue(String);
impl SecretValue {
    pub fn new(value: String) -> Self {
        Self(value)
    }
    pub fn expose(&self) -> &str {
        &self.0
    }
}
impl fmt::Debug for SecretValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretValue([REDACTED])")
    }
}

#[derive(Clone, Debug)]
pub struct LogEntry {
    pub sequence: u64,
    pub timestamp: String,
    pub level: String,
    pub subsystem: String,
    pub message: String,
    pub details: String,
}
impl LogEntry {
    fn bytes(&self) -> usize {
        self.timestamp.len()
            + self.level.len()
            + self.subsystem.len()
            + self.message.len()
            + self.details.len()
            + 64
    }
}
#[derive(Clone, Copy, Debug)]
pub enum Level {
    Info,
    Warn,
    Error,
}
#[derive(Clone, Copy, Debug)]
pub enum Subsystem {
    App,
    Settings,
    Credentials,
    Twitch,
    OAuth,
    Provider,
}
#[derive(Clone, Copy, Debug)]
pub enum Event {
    Started,
    Stopping,
    SettingsSaved,
    SettingsRejected,
    CredentialSaved,
    CredentialRemoved,
    CredentialStoreUnavailable,
    AuthorizationStarted,
    AuthorizationCancelled,
    AuthorizationExpired,
    AuthorizationFailed,
    Authorized,
    TokenRefreshed,
    ReconnectRequired,
    Connected,
    Disconnected,
    Reconnecting,
    Paused,
    Resumed,
    RequestStarted,
    RequestSucceeded,
    RequestFailed,
    ReplySent,
    ReplySkipped,
    MemoryCleared,
    LogsCleared,
    PreviousCrash,
}
impl Event {
    fn message(self) -> &'static str {
        match self {
            Self::Started => "Application started",
            Self::Stopping => "Application stopping",
            Self::SettingsSaved => "Settings saved",
            Self::SettingsRejected => "Settings update rejected",
            Self::CredentialSaved => "Credential saved",
            Self::CredentialRemoved => "Credential removed",
            Self::CredentialStoreUnavailable => "Local credential files unavailable",
            Self::AuthorizationStarted => "Twitch authorization started",
            Self::AuthorizationCancelled => "Twitch authorization cancelled",
            Self::AuthorizationExpired => "Twitch authorization expired",
            Self::AuthorizationFailed => "Twitch authorization failed",
            Self::Authorized => "Twitch account authorized",
            Self::TokenRefreshed => "Twitch credentials refreshed",
            Self::ReconnectRequired => "Twitch authorization must be renewed",
            Self::Connected => "Twitch connected",
            Self::Disconnected => "Twitch disconnected",
            Self::Reconnecting => "Twitch reconnecting",
            Self::Paused => "Bot paused",
            Self::Resumed => "Bot resumed",
            Self::RequestStarted => "AI request started",
            Self::RequestSucceeded => "AI request completed",
            Self::RequestFailed => "AI request failed",
            Self::ReplySent => "Chat reply delivered",
            Self::ReplySkipped => "Chat reply skipped",
            Self::MemoryCleared => "Conversation memory cleared",
            Self::LogsCleared => "Log view cleared",
            Self::PreviousCrash => "Previous application session ended unexpectedly",
        }
    }
}

/// Only these classified reasons may enter logs. Raw upstream error messages are excluded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailureKind {
    Authentication,
    RateLimit,
    NotFound,
    InvalidRequest,
    Server,
    Timeout,
    Network,
    ResponseRead,
    ResponseTooLarge,
    InvalidJson,
    NoText,
    ProviderReported,
}
#[derive(Clone, Copy, Debug)]
pub enum Field<'a> {
    HttpStatus(u16),
    LatencyMs(u64),
    Model(&'a str),
    RequestId(&'a str),
    Failure(FailureKind),
    Provider(&'a str),
}

fn identifier(value: &str, limit: usize) -> Option<&str> {
    if value.is_empty()
        || value.len() > limit
        || value.starts_with("sk-")
        || value.starts_with("sk_")
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.:/".contains(&b))
    {
        None
    } else {
        Some(value)
    }
}
fn fields_text(fields: &[Field<'_>]) -> String {
    fields
        .iter()
        .take(8)
        .filter_map(|field| match field {
            Field::HttpStatus(n) => Some(format!("http_status={n}")),
            Field::LatencyMs(n) => Some(format!("latency_ms={n}")),
            Field::Failure(kind) => Some(format!("failure={kind:?}")),
            Field::Model(value) => identifier(value, 200).map(|v| format!("model={v}")),
            Field::RequestId(value) => identifier(value, 128).map(|v| format!("request_id={v}")),
            Field::Provider(value) => match *value {
                "openai" | "anthropic" | "openrouter" | "compatible" => {
                    Some(format!("provider={value}"))
                }
                _ => None,
            },
        })
        .collect::<Vec<_>>()
        .join(" ")
}

// Gregorian UTC date conversion, avoiding a clock-formatting dependency in the runtime.
fn timestamp(now: std::time::Duration) -> String {
    let seconds = now.as_secs();
    let days = (seconds / 86_400).min(i64::MAX as u64 - 719_468) as i64 + 719_468;
    let era = days / 146_097;
    let doe = days - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    let year = year + i64::from(month <= 2);
    let time = seconds % 86_400;
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}.{:03} UTC",
        time / 3_600,
        time / 60 % 60,
        time % 60,
        now.subsec_millis()
    )
}
#[derive(Default)]
struct Ring {
    entries: VecDeque<LogEntry>,
    bytes: usize,
    sequence: u64,
}
#[derive(Default)]
struct Shared {
    ring: Mutex<Ring>,
    dropped: AtomicU64,
}
#[derive(Clone, Default)]
pub struct Diagnostics(Arc<Shared>);
impl Diagnostics {
    pub fn new() -> Self {
        Self::default()
    }
    /// Producers never wait for UI/export readers. A contended event is counted and dropped.
    pub fn emit(&self, level: Level, subsystem: Subsystem, event: Event, fields: &[Field<'_>]) {
        let Ok(mut ring) = self.0.ring.try_lock() else {
            self.0.dropped.fetch_add(1, Ordering::Relaxed);
            return;
        };
        ring.sequence = ring.sequence.saturating_add(1);
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        let entry = LogEntry {
            sequence: ring.sequence,
            timestamp: timestamp(now),
            level: format!("{level:?}"),
            subsystem: format!("{subsystem:?}"),
            message: event.message().into(),
            details: fields_text(fields),
        };
        ring.bytes += entry.bytes();
        ring.entries.push_back(entry);
        while ring.entries.len() > MAX_ENTRIES || ring.bytes > MAX_BYTES {
            if let Some(old) = ring.entries.pop_front() {
                ring.bytes = ring.bytes.saturating_sub(old.bytes());
            }
        }
    }
    pub fn snapshot(&self) -> Vec<LogEntry> {
        self.read_after(0)
    }
    pub fn read_after(&self, sequence: u64) -> Vec<LogEntry> {
        self.0
            .ring
            .lock()
            .map(|ring| {
                ring.entries
                    .iter()
                    .filter(|entry| entry.sequence > sequence)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }
    pub fn clear(&self) {
        if let Ok(mut ring) = self.0.ring.lock() {
            ring.entries.clear();
            ring.bytes = 0;
        }
    }
    pub fn dropped(&self) -> u64 {
        self.0.dropped.load(Ordering::Relaxed)
    }
    pub fn export(&self) -> String {
        let mut output = format!(
            "MPD Bot session diagnostics; dropped events: {}\n",
            self.dropped()
        );
        for entry in self.snapshot() {
            output.push_str(&format!(
                "{} [{}] {}: {} {}\n",
                entry.timestamp, entry.level, entry.subsystem, entry.message, entry.details
            ));
        }
        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn flood_and_clear_are_bounded_and_sequence_does_not_reset() {
        let log = Diagnostics::new();
        for _ in 0..10_000 {
            log.emit(Level::Info, Subsystem::App, Event::Started, &[]);
        }
        assert_eq!(log.snapshot().len(), MAX_ENTRIES);
        assert!(log.0.ring.lock().unwrap().bytes <= MAX_BYTES);
        assert_eq!(log.read_after(9_999).len(), 1);
        log.clear();
        log.emit(Level::Info, Subsystem::App, Event::Started, &[]);
        assert_eq!(log.snapshot()[0].sequence, 10_001);
    }
    #[test]
    fn timestamps_are_readable_utc() {
        assert_eq!(
            timestamp(std::time::Duration::ZERO),
            "1970-01-01 00:00:00.000 UTC"
        );
        assert_eq!(
            timestamp(std::time::Duration::from_secs(951_782_400)),
            "2000-02-29 00:00:00.000 UTC"
        );
    }
    #[test]
    fn producer_drops_instead_of_waiting() {
        let log = Diagnostics::new();
        let _guard = log.0.ring.lock().unwrap();
        log.emit(Level::Warn, Subsystem::App, Event::Stopping, &[]);
        assert_eq!(log.dropped(), 1);
    }
    #[test]
    fn secret_debug_and_invalid_fields_are_redacted() {
        let sentinel = "sk-SECRET_SENTINEL";
        assert!(!format!("{:?}", SecretValue::new(sentinel.into())).contains(sentinel));
        let log = Diagnostics::new();
        log.emit(
            Level::Error,
            Subsystem::Provider,
            Event::RequestFailed,
            &[
                Field::Model(sentinel),
                Field::RequestId("bad\nSECRET_SENTINEL"),
                Field::Provider(sentinel),
            ],
        );
        assert!(!log.export().contains("SECRET_SENTINEL"));
    }
}
