use chrono::{DateTime, SecondsFormat, Utc};
use rmcp::schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Hash, Default)]
#[serde(rename_all = "kebab-case")]
pub enum MemoryKind {
    #[default]
    Fact,
    Preference,
    Goal,
    Project,
    Procedure,
    Event,
    Other,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Hash, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Provenance {
    #[default]
    UserStated,
    AgentInferred,
    Imported,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Memory {
    pub id: String,
    pub namespace: String,
    pub content: String,
    pub title: Option<String>,
    pub kind: MemoryKind,
    pub tags: Vec<String>,
    pub confidence: f32,
    pub importance: f32,
    pub provenance: Provenance,
    pub source_client: String,
    pub created_at: String,
    pub updated_at: String,
    pub expires_at: Option<String>,
    pub revision: i64,
    pub deleted_at: Option<String>,
    #[serde(skip, default)]
    #[schemars(skip)]
    pub version_event_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SearchInput {
    /// Natural-language or keyword query (1–2,000 characters).
    pub query: String,
    pub namespace: Option<String>,
    pub limit: Option<u8>,
    pub kinds: Option<Vec<MemoryKind>>,
    pub tags_any: Option<Vec<String>>,
    pub include_expired: Option<bool>,
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GetInput {
    pub memory_id: String,
    pub namespace: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SaveInput {
    pub content: String,
    pub namespace: Option<String>,
    pub title: Option<String>,
    pub kind: Option<MemoryKind>,
    pub tags: Option<Vec<String>>,
    pub confidence: Option<f32>,
    pub importance: Option<f32>,
    pub provenance: Option<Provenance>,
    pub expires_at: Option<String>,
    pub client_request_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateInput {
    pub memory_id: String,
    pub expected_revision: i64,
    pub content: Option<String>,
    #[serde(default, deserialize_with = "deserialize_nullable")]
    pub title: Option<Option<String>>,
    pub kind: Option<MemoryKind>,
    pub tags: Option<Vec<String>>,
    pub confidence: Option<f32>,
    pub importance: Option<f32>,
    #[serde(default, deserialize_with = "deserialize_nullable")]
    pub expires_at: Option<Option<String>>,
}

fn deserialize_nullable<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DeleteInput {
    pub memory_id: String,
    pub expected_revision: i64,
    pub namespace: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SearchOutput {
    pub results: Vec<MemoryResult>,
    pub next_cursor: Option<String>,
    pub search_mode: SearchMode,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SearchMode {
    Hybrid,
    Lexical,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct MemoryResult {
    #[serde(flatten)]
    pub memory: Memory,
    pub relevance: f32,
    pub match_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct GetOutput {
    pub memory: Option<Memory>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SaveOutput {
    pub memory: Memory,
    pub created: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct UpdateOutput {
    pub memory: Memory,
    pub updated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DeleteOutput {
    pub memory_id: String,
    pub deleted: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tombstone {
    pub memory_id: String,
    pub namespace: String,
    pub revision: i64,
    pub deleted_at: String,
    pub version_event_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MutationOperation {
    Save,
    Update,
    Delete,
    Resolve,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "kebab-case")]
pub enum MutationPayload {
    Record(Memory),
    Tombstone(Tombstone),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdempotencyEntry {
    pub client_id: String,
    pub client_request_id: String,
    pub request_fingerprint: String,
    pub result_json: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MutationEvent {
    pub event_id: String,
    pub origin_replica_id: String,
    pub origin_seq: i64,
    pub owner_id: String,
    pub client_id: String,
    pub memory_id: String,
    pub namespace: String,
    pub operation: MutationOperation,
    pub base_event_id: Option<String>,
    pub payload: MutationPayload,
    pub created_at: String,
    pub idempotency: Option<IdempotencyEntry>,
}

#[derive(Debug, Clone, Error)]
pub enum AppError {
    #[error("invalid argument")]
    InvalidArgument { field: String, message: String },
    #[error("forbidden")]
    Forbidden,
    #[error("not found")]
    NotFound,
    #[error("revision conflict")]
    RevisionConflict { current_revision: i64 },
    #[error("replication conflict")]
    ReplicationConflict,
    #[error("quota exceeded")]
    QuotaExceeded,
    #[error("temporarily unavailable")]
    TemporarilyUnavailable,
    #[error("idempotency key already used with different request")]
    IdempotencyConflict,
}

impl AppError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidArgument { .. } => "invalid_argument",
            Self::Forbidden => "forbidden",
            Self::NotFound => "not_found",
            Self::RevisionConflict { .. } => "revision_conflict",
            Self::ReplicationConflict => "replication_conflict",
            Self::QuotaExceeded => "quota_exceeded",
            Self::TemporarilyUnavailable => "temporarily_unavailable",
            Self::IdempotencyConflict => "invalid_argument",
        }
    }

    pub fn safe_message(&self) -> String {
        match self {
            Self::InvalidArgument { message, .. } => message.clone(),
            Self::Forbidden => "The operation is not permitted for this credential.".into(),
            Self::NotFound => "The requested memory was not found.".into(),
            Self::RevisionConflict { .. } => "The memory changed since the supplied revision.".into(),
            Self::ReplicationConflict => "This memory has an unresolved replication conflict.".into(),
            Self::QuotaExceeded => "The owner's memory quota has been reached.".into(),
            Self::TemporarilyUnavailable => "The service is temporarily unavailable; retry on the same route with the same idempotency key.".into(),
            Self::IdempotencyConflict => "client_request_id was already used for a different request.".into(),
        }
    }
}

pub fn invalid(field: &str, message: &str) -> AppError {
    AppError::InvalidArgument {
        field: field.to_owned(),
        message: message.to_owned(),
    }
}

pub fn normalize_namespace(value: Option<&str>) -> Result<String, AppError> {
    let namespace = value.unwrap_or("shared").trim();
    let length = namespace.chars().count();
    if !(1..=80).contains(&length) {
        return Err(invalid(
            "namespace",
            "namespace must contain 1 to 80 characters",
        ));
    }
    if namespace.chars().any(char::is_control) {
        return Err(invalid(
            "namespace",
            "namespace contains a control character",
        ));
    }
    Ok(namespace.to_owned())
}

pub fn validate_content(content: &str) -> Result<String, AppError> {
    let trimmed = content.trim();
    let length = trimmed.chars().count();
    if length == 0 {
        return Err(invalid("content", "content must not be empty"));
    }
    if length > 8000 {
        return Err(invalid(
            "content",
            "content must not exceed 8,000 characters",
        ));
    }
    Ok(trimmed.to_owned())
}

pub fn validate_title(title: Option<&str>) -> Result<Option<String>, AppError> {
    let Some(title) = title else {
        return Ok(None);
    };
    let title = title.trim();
    if title.chars().count() > 200 {
        return Err(invalid("title", "title must not exceed 200 characters"));
    }
    if title.is_empty() {
        return Ok(None);
    }
    Ok(Some(title.to_owned()))
}

pub fn normalize_tags(tags: Option<&[String]>) -> Result<Vec<String>, AppError> {
    let Some(tags) = tags else {
        return Ok(Vec::new());
    };
    if tags.len() > 20 {
        return Err(invalid("tags", "at most 20 tags are allowed"));
    }
    let mut normalized = Vec::new();
    for tag in tags {
        let value = normalize_tag(tag);
        let length = value.chars().count();
        if !(1..=64).contains(&length) {
            return Err(invalid(
                "tags",
                "each tag must normalize to 1 to 64 characters",
            ));
        }
        if !normalized.contains(&value) {
            normalized.push(value);
        }
    }
    Ok(normalized)
}

pub fn normalize_tag(tag: &str) -> String {
    let mut out = String::new();
    let mut separator = false;
    for ch in tag.trim().chars().flat_map(char::to_lowercase) {
        if ch.is_alphanumeric() {
            if separator && !out.is_empty() {
                out.push('-');
            }
            out.push(ch);
            separator = false;
        } else {
            separator = true;
        }
    }
    out
}

pub fn validate_confidence(value: Option<f32>, default: f32, field: &str) -> Result<f32, AppError> {
    let value = value.unwrap_or(default);
    if !value.is_finite() || !(0.0..=1.0).contains(&value) {
        return Err(invalid(field, "value must be between 0 and 1"));
    }
    Ok(value)
}

pub fn parse_expiry(value: Option<&str>) -> Result<Option<String>, AppError> {
    value
        .map(|raw| {
            let parsed = DateTime::parse_from_rfc3339(raw)
                .map_err(|_| invalid("expires_at", "expires_at must be an RFC 3339 timestamp"))?;
            Ok(parsed
                .with_timezone(&Utc)
                .to_rfc3339_opts(SecondsFormat::Millis, true))
        })
        .transpose()
}

pub fn validate_uuid(value: &str, field: &str) -> Result<String, AppError> {
    uuid::Uuid::parse_str(value)
        .map(|uuid| uuid.to_string())
        .map_err(|_| invalid(field, "must be a valid UUID"))
}

pub fn validate_revision(revision: i64) -> Result<i64, AppError> {
    if revision < 1 {
        return Err(invalid(
            "expected_revision",
            "expected_revision must be a positive integer",
        ));
    }
    Ok(revision)
}

pub fn validate_request_id(value: Option<&str>) -> Result<Option<String>, AppError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let value = value.trim();
    if value.is_empty() || value.chars().count() > 128 || value.chars().any(char::is_control) {
        return Err(invalid(
            "client_request_id",
            "client_request_id must contain 1 to 128 non-control characters",
        ));
    }
    Ok(Some(value.to_owned()))
}

pub fn now_rfc3339() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits_and_tag_normalization_are_enforced() {
        assert_eq!(
            normalize_tag(" Learning App / Voice "),
            "learning-app-voice"
        );
        assert_eq!(normalize_namespace(None).expect("default"), "shared");
        assert!(validate_content("  \n ").is_err());
        assert!(validate_content(&"x".repeat(8001)).is_err());
        assert_eq!(
            normalize_tags(Some(&["Topic A".into(), "topic-a".into()])).expect("tags"),
            vec!["topic-a"]
        );
    }

    #[test]
    fn update_null_distinguishes_omission_from_clear() {
        let omitted: UpdateInput = serde_json::from_str(r#"{"memory_id":"00000000-0000-4000-8000-000000000000","expected_revision":1,"content":"new"}"#).expect("parse omitted");
        let cleared: UpdateInput = serde_json::from_str(r#"{"memory_id":"00000000-0000-4000-8000-000000000000","expected_revision":1,"title":null}"#).expect("parse clear");
        assert_eq!(omitted.title, None);
        assert_eq!(cleared.title, Some(None));
    }
}
