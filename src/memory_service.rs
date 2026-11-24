use std::sync::Arc;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::Utc;
use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    auth::AuthContext,
    domain::{
        AppError, DeleteInput, DeleteOutput, GetInput, GetOutput, Memory, SaveInput, SaveOutput,
        SearchInput, SearchOutput, UpdateInput, UpdateOutput, invalid, normalize_namespace,
        normalize_tags, now_rfc3339, parse_expiry, validate_confidence, validate_content,
        validate_request_id, validate_revision, validate_title, validate_uuid,
    },
    embedding::EmbeddingProvider,
    store::{SearchPage, SearchRequest, Store, UpdatePatch},
};

type HmacSha256 = Hmac<Sha256>;

#[derive(Clone)]
pub struct MemoryService {
    store: Store,
    cursor_secret: Vec<u8>,
}

#[derive(Debug, Serialize, Deserialize)]
struct CursorPayload {
    owner_id: String,
    client_id: String,
    namespace: String,
    filter_digest: String,
    offset: usize,
    expires_at: i64,
}

impl MemoryService {
    pub fn new(store: Store, cursor_secret: Vec<u8>) -> Self {
        Self {
            store,
            cursor_secret,
        }
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    #[allow(dead_code)] // The production CLI remains lexical until a local model is configured.
    pub fn with_embedding_provider(
        store: Store,
        cursor_secret: Vec<u8>,
        provider: Arc<dyn EmbeddingProvider>,
    ) -> Result<Self, AppError> {
        store.configure_embeddings(provider)?;
        Ok(Self::new(store, cursor_secret))
    }

    pub fn search(&self, auth: &AuthContext, input: SearchInput) -> Result<SearchOutput, AppError> {
        require_scope(auth, "memory:read")?;
        let query = input.query.trim();
        let query_length = query.chars().count();
        if !(1..=2000).contains(&query_length) {
            return Err(invalid("query", "query must contain 1 to 2,000 characters"));
        }
        let namespace = normalize_namespace(input.namespace.as_deref())?;
        authorize_namespace(auth, &namespace)?;
        let limit = usize::from(input.limit.unwrap_or(8));
        if !(1..=20).contains(&limit) {
            return Err(invalid("limit", "limit must be between 1 and 20"));
        }
        if input.include_expired.unwrap_or(false) {
            return Err(AppError::Forbidden);
        }
        if input.kinds.as_ref().is_some_and(|items| items.len() > 7) {
            return Err(invalid("kinds", "at most 7 kinds may be supplied"));
        }
        let normalized_tags = match input.tags_any.as_deref() {
            Some(tags) => {
                if tags.is_empty() {
                    return Err(invalid(
                        "tags_any",
                        "tags_any must contain at least one tag when supplied",
                    ));
                }
                Some(normalize_tags(Some(tags))?)
            }
            None => None,
        };
        let filter_digest = digest_json(&(
            query.to_lowercase(),
            &namespace,
            limit,
            &input.kinds,
            &normalized_tags,
            input.include_expired.unwrap_or(false),
        ));
        let offset = if let Some(token) = input.cursor.as_deref() {
            let payload = self.decode_cursor(token)?;
            if payload.owner_id != auth.owner_id
                || payload.client_id != auth.client_id
                || payload.namespace != namespace
                || payload.filter_digest != filter_digest
            {
                return Err(invalid(
                    "cursor",
                    "cursor does not match this search request",
                ));
            }
            if payload.expires_at < Utc::now().timestamp() {
                return Err(invalid("cursor", "cursor has expired"));
            }
            payload.offset
        } else {
            0
        };
        let query_embedding = self.store.embed_query(query);
        let mut output = self.store.search_records_semantic(SearchRequest {
            auth,
            namespace: &namespace,
            query,
            kinds: input.kinds.as_deref(),
            tags_any: normalized_tags.as_deref(),
            query_embedding: query_embedding.as_deref(),
            page: SearchPage { offset, limit },
        })?;
        if output.next_cursor.is_some() {
            output.next_cursor = Some(self.encode_cursor(CursorPayload {
                owner_id: auth.owner_id.clone(),
                client_id: auth.client_id.clone(),
                namespace,
                filter_digest,
                offset: offset.saturating_add(output.results.len()),
                expires_at: Utc::now().timestamp() + 3600,
            })?);
        }
        Ok(output)
    }

    pub fn get(&self, auth: &AuthContext, input: GetInput) -> Result<GetOutput, AppError> {
        require_scope(auth, "memory:read")?;
        let id = validate_uuid(&input.memory_id, "memory_id")?;
        let namespace = normalize_namespace(input.namespace.as_deref())?;
        if !auth.allows_namespace(&namespace) {
            return Ok(GetOutput { memory: None });
        }
        Ok(GetOutput {
            memory: self.store.get_record(auth, &id, &namespace)?,
        })
    }

    pub fn save(&self, auth: &AuthContext, input: SaveInput) -> Result<SaveOutput, AppError> {
        require_scope(auth, "memory:write")?;
        let namespace = normalize_namespace(input.namespace.as_deref())?;
        authorize_namespace(auth, &namespace)?;
        let content = validate_content(&input.content)?;
        let title = validate_title(input.title.as_deref())?;
        let tags = normalize_tags(input.tags.as_deref())?;
        let confidence = validate_confidence(input.confidence, 1.0, "confidence")?;
        let importance = validate_confidence(input.importance, 0.5, "importance")?;
        let expires_at = parse_expiry(input.expires_at.as_deref())?;
        if expires_at.as_deref().is_some_and(is_expiry_in_past) {
            return Err(invalid("expires_at", "expires_at must be in the future"));
        }
        let request_id = validate_request_id(input.client_request_id.as_deref())?;
        let kind = input.kind.unwrap_or_default();
        let provenance = input.provenance.unwrap_or_default();
        let normalized_request = (
            &namespace,
            &content,
            &title,
            kind,
            &tags,
            confidence,
            importance,
            provenance,
            &expires_at,
        );
        let fingerprint = digest_json(&normalized_request);
        let now = now_rfc3339();
        let memory = Memory {
            id: Uuid::now_v7().to_string(),
            namespace,
            content,
            title,
            kind,
            tags,
            confidence,
            importance,
            provenance,
            source_client: auth.client_id.clone(),
            created_at: now.clone(),
            updated_at: now,
            expires_at,
            revision: 1,
            deleted_at: None,
            version_event_id: None,
        };
        let (memory, created) =
            self.store
                .save_record(auth, memory, request_id.as_deref(), &fingerprint)?;
        Ok(SaveOutput { memory, created })
    }

    pub fn update(&self, auth: &AuthContext, input: UpdateInput) -> Result<UpdateOutput, AppError> {
        require_scope(auth, "memory:write")?;
        let id = validate_uuid(&input.memory_id, "memory_id")?;
        validate_revision(input.expected_revision)?;
        let namespace = self
            .store
            .namespace_for_record(auth, &id)?
            .ok_or(AppError::NotFound)?;
        if !auth.allows_namespace(&namespace) {
            return Err(AppError::NotFound);
        }
        if input.content.is_none()
            && input.title.is_none()
            && input.kind.is_none()
            && input.tags.is_none()
            && input.confidence.is_none()
            && input.importance.is_none()
            && input.expires_at.is_none()
        {
            return Err(invalid(
                "update",
                "at least one mutable field must be supplied",
            ));
        }
        let content = input.content.as_deref().map(validate_content).transpose()?;
        let title = input
            .title
            .as_ref()
            .map(|value| validate_title(value.as_deref()))
            .transpose()?;
        let tags = input
            .tags
            .as_deref()
            .map(|value| normalize_tags(Some(value)))
            .transpose()?;
        let confidence = input
            .confidence
            .map(|value| validate_confidence(Some(value), 0.5, "confidence"))
            .transpose()?;
        let importance = input
            .importance
            .map(|value| validate_confidence(Some(value), 0.5, "importance"))
            .transpose()?;
        let expires_at = match input.expires_at.as_ref() {
            Some(Some(value)) => Some(Some(
                parse_expiry(Some(value))?
                    .ok_or_else(|| invalid("expires_at", "invalid expiry"))?,
            )),
            Some(None) => Some(None),
            None => None,
        };
        if expires_at
            .as_ref()
            .and_then(|value| value.as_deref())
            .is_some_and(is_expiry_in_past)
        {
            return Err(invalid("expires_at", "expires_at must be in the future"));
        }
        let patch = UpdatePatch {
            content,
            title,
            kind: input.kind,
            tags,
            confidence,
            importance,
            expires_at,
        };
        self.store
            .update_record(auth, &id, &namespace, input.expected_revision, patch)
    }

    pub fn delete(&self, auth: &AuthContext, input: DeleteInput) -> Result<DeleteOutput, AppError> {
        require_scope(auth, "memory:delete")?;
        let id = validate_uuid(&input.memory_id, "memory_id")?;
        validate_revision(input.expected_revision)?;
        let namespace = normalize_namespace(input.namespace.as_deref())?;
        if !auth.allows_namespace(&namespace) {
            return Err(AppError::NotFound);
        }
        self.store
            .delete_record(auth, &id, &namespace, input.expected_revision)
    }

    fn encode_cursor(&self, payload: CursorPayload) -> Result<String, AppError> {
        let json = serde_json::to_vec(&payload).map_err(|_| AppError::TemporarilyUnavailable)?;
        let mut mac = HmacSha256::new_from_slice(&self.cursor_secret)
            .map_err(|_| AppError::TemporarilyUnavailable)?;
        mac.update(&json);
        let signature = mac.finalize().into_bytes();
        Ok(format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(json),
            URL_SAFE_NO_PAD.encode(signature)
        ))
    }

    fn decode_cursor(&self, cursor: &str) -> Result<CursorPayload, AppError> {
        let (encoded, signature) = cursor
            .split_once('.')
            .ok_or_else(|| invalid("cursor", "cursor is malformed"))?;
        let json = URL_SAFE_NO_PAD
            .decode(encoded)
            .map_err(|_| invalid("cursor", "cursor is malformed"))?;
        let signature = URL_SAFE_NO_PAD
            .decode(signature)
            .map_err(|_| invalid("cursor", "cursor is malformed"))?;
        let mut mac = HmacSha256::new_from_slice(&self.cursor_secret)
            .map_err(|_| AppError::TemporarilyUnavailable)?;
        mac.update(&json);
        mac.verify_slice(&signature)
            .map_err(|_| invalid("cursor", "cursor signature is invalid"))?;
        serde_json::from_slice(&json).map_err(|_| invalid("cursor", "cursor is malformed"))
    }
}

fn require_scope(auth: &AuthContext, scope: &str) -> Result<(), AppError> {
    if auth.has_scope(scope) {
        Ok(())
    } else {
        Err(AppError::Forbidden)
    }
}

fn authorize_namespace(auth: &AuthContext, namespace: &str) -> Result<(), AppError> {
    if auth.allows_namespace(namespace) {
        Ok(())
    } else {
        Err(AppError::Forbidden)
    }
}

fn digest_json<T: Serialize>(value: &T) -> String {
    let serialized = serde_json::to_vec(value).unwrap_or_default();
    hex::encode(Sha256::digest(serialized))
}

fn is_expiry_in_past(value: &str) -> bool {
    chrono::DateTime::parse_from_rfc3339(value)
        .map(|time| time <= Utc::now())
        .unwrap_or(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::SearchMode;
    use std::collections::HashSet;
    use tempfile::tempdir;

    fn auth() -> AuthContext {
        AuthContext {
            owner_id: "owner".into(),
            client_id: "cue".into(),
            scopes: HashSet::from([
                "memory:read".into(),
                "memory:write".into(),
                "memory:delete".into(),
            ]),
            allowed_namespaces: HashSet::from(["shared".into()]),
            token_fingerprint: "hash".into(),
        }
    }

    #[test]
    fn save_search_and_signed_cursor_work() {
        let dir = tempdir().expect("temp dir");
        let store = Store::open(&dir.path().join("db.duckdb"), "replica", 100).expect("store");
        let service = MemoryService::new(store, vec![7; 32]);
        service
            .save(
                &auth(),
                SaveInput {
                    content: "Cantonese voice practice preference".into(),
                    namespace: None,
                    title: None,
                    kind: None,
                    tags: Some(vec!["learning app".into()]),
                    confidence: None,
                    importance: None,
                    provenance: None,
                    expires_at: None,
                    client_request_id: None,
                },
            )
            .expect("save");
        let result = service
            .search(
                &auth(),
                SearchInput {
                    query: "voice practice".into(),
                    namespace: None,
                    limit: Some(1),
                    kinds: None,
                    tags_any: Some(vec!["learning-app".into()]),
                    include_expired: None,
                    cursor: None,
                },
            )
            .expect("search");
        assert_eq!(result.results.len(), 1);
        assert_eq!(result.search_mode, SearchMode::Lexical);
    }

    struct TestEmbeddingProvider;

    impl EmbeddingProvider for TestEmbeddingProvider {
        fn model(&self) -> crate::embedding::EmbeddingModel {
            crate::embedding::EmbeddingModel {
                name: "local-semantic-test".into(),
                version: "1".into(),
                dimension: 3,
            }
        }

        fn embed(&self, text: &str) -> Result<Vec<f32>, String> {
            let text = text.to_lowercase();
            if ["automobile", "vehicle", "toyota", "mercedes", "car"]
                .iter()
                .any(|term| text.contains(term))
            {
                Ok(vec![1.0, 0.0, 0.0])
            } else if ["cantonese", "tone", "pitch"]
                .iter()
                .any(|term| text.contains(term))
            {
                Ok(vec![0.0, 1.0, 0.0])
            } else {
                Ok(vec![0.0, 0.0, 1.0])
            }
        }
    }

    fn search_for_vehicle_in(
        service: &MemoryService,
        auth: &AuthContext,
        namespace: Option<&str>,
    ) -> SearchOutput {
        service
            .search(
                auth,
                SearchInput {
                    query: "Find the automobile owned by this person".into(),
                    namespace: namespace.map(str::to_owned),
                    limit: Some(8),
                    kinds: None,
                    tags_any: None,
                    include_expired: None,
                    cursor: None,
                },
            )
            .expect("semantic search")
    }

    fn search_for_vehicle(service: &MemoryService) -> SearchOutput {
        search_for_vehicle_in(service, &auth(), None)
    }

    #[test]
    fn semantic_search_rebuilds_after_restart_and_tracks_update_delete() {
        let dir = tempdir().expect("temp dir");
        let path = dir.path().join("semantic.duckdb");
        let store = Store::open(&path, "replica-semantic", 100).expect("store");
        let vss_loaded = store.vss_available();
        let service = MemoryService::with_embedding_provider(
            store,
            vec![7; 32],
            Arc::new(TestEmbeddingProvider),
        )
        .expect("configure local test embeddings");
        let saved = service
            .save(
                &auth(),
                SaveInput {
                    content: "Iakov owns a Toyota station wagon.".into(),
                    namespace: None,
                    title: None,
                    kind: None,
                    tags: Some(vec!["vehicle".into()]),
                    confidence: None,
                    importance: None,
                    provenance: None,
                    expires_at: None,
                    client_request_id: Some("semantic-save-1".into()),
                },
            )
            .expect("save vehicle memory");
        let mut other_owner = auth();
        other_owner.owner_id = "different-owner".into();
        let other_owner_memory = service
            .save(
                &other_owner,
                SaveInput {
                    content: "Maria owns a Mercedes-Benz.".into(),
                    namespace: None,
                    title: None,
                    kind: None,
                    tags: Some(vec!["vehicle".into()]),
                    confidence: None,
                    importance: None,
                    provenance: None,
                    expires_at: None,
                    client_request_id: Some("semantic-save-other-owner".into()),
                },
            )
            .expect("save another owner's memory");
        let mut private_auth = auth();
        private_auth.allowed_namespaces.insert("private".into());
        let private_memory = service
            .save(
                &private_auth,
                SaveInput {
                    content: "Private note: another Mercedes is present.".into(),
                    namespace: Some("private".into()),
                    title: None,
                    kind: None,
                    tags: Some(vec!["vehicle".into()]),
                    confidence: None,
                    importance: None,
                    provenance: None,
                    expires_at: None,
                    client_request_id: Some("semantic-save-private".into()),
                },
            )
            .expect("save private namespace memory");
        service
            .save(
                &auth(),
                SaveInput {
                    content: "Cantonese tone and pitch practice.".into(),
                    namespace: None,
                    title: None,
                    kind: None,
                    tags: Some(vec!["cantonese".into()]),
                    confidence: None,
                    importance: None,
                    provenance: None,
                    expires_at: None,
                    client_request_id: Some("semantic-save-2".into()),
                },
            )
            .expect("save unrelated memory");

        let found = search_for_vehicle(&service);
        assert_eq!(found.search_mode, SearchMode::Hybrid);
        assert_eq!(
            found.results.len(),
            1,
            "unexpected candidates: {:?}",
            found
                .results
                .iter()
                .map(|result| (
                    result.memory.id.as_str(),
                    result.memory.content.as_str(),
                    result.relevance,
                    result.match_reason.as_deref(),
                ))
                .collect::<Vec<_>>()
        );
        assert_eq!(found.results[0].memory.id, saved.memory.id);
        assert_eq!(
            found.results[0].match_reason.as_deref(),
            Some("semantic vector similarity")
        );

        let other_owner_found = search_for_vehicle_in(&service, &other_owner, None);
        assert_eq!(other_owner_found.results.len(), 1);
        assert_eq!(
            other_owner_found.results[0].memory.id,
            other_owner_memory.memory.id
        );
        let private_hidden = search_for_vehicle(&service);
        assert!(
            private_hidden
                .results
                .iter()
                .all(|result| result.memory.id != private_memory.memory.id)
        );
        let private_found = search_for_vehicle_in(&service, &private_auth, Some("private"));
        assert_eq!(private_found.results.len(), 1);
        assert_eq!(private_found.results[0].memory.id, private_memory.memory.id);

        let engine = service.store().search_engine();
        if vss_loaded {
            assert_eq!(engine, "duckdb_vss_hnsw");
        } else {
            assert_eq!(engine, "exact_scoped_vector_scan");
        }
        drop(service);

        let reopened = Store::open(&path, "replica-semantic", 100).expect("reopen store");
        let reopened_vss_loaded = reopened.vss_available();
        let service = MemoryService::with_embedding_provider(
            reopened,
            vec![7; 32],
            Arc::new(TestEmbeddingProvider),
        )
        .expect("rebuild persisted local embeddings");
        assert_eq!(
            search_for_vehicle(&service).results[0].memory.id,
            saved.memory.id
        );
        assert_eq!(
            service.store().search_engine(),
            if reopened_vss_loaded {
                "duckdb_vss_hnsw"
            } else {
                "exact_scoped_vector_scan"
            }
        );

        let updated = service
            .update(
                &auth(),
                UpdateInput {
                    memory_id: saved.memory.id.clone(),
                    expected_revision: 1,
                    content: Some("Cantonese practice follows a high-tone melody.".into()),
                    title: None,
                    kind: None,
                    tags: Some(vec!["cantonese".into()]),
                    confidence: None,
                    importance: None,
                    expires_at: None,
                },
            )
            .expect("update vehicle memory");
        assert_eq!(updated.memory.revision, 2);
        let after_update = search_for_vehicle(&service);
        assert!(
            after_update.results.is_empty(),
            "stale candidate in {}: {:?}",
            service.store().search_engine(),
            after_update
                .results
                .iter()
                .map(|result| (
                    result.memory.content.as_str(),
                    result.relevance,
                    result.match_reason.as_deref(),
                ))
                .collect::<Vec<_>>()
        );

        service
            .delete(
                &auth(),
                crate::domain::DeleteInput {
                    memory_id: saved.memory.id,
                    expected_revision: 2,
                    namespace: None,
                },
            )
            .expect("delete vehicle memory");
        assert!(search_for_vehicle(&service).results.is_empty());
    }

    #[test]
    fn update_uses_record_namespace_without_leaking_disallowed_records() {
        let dir = tempdir().expect("temp dir");
        let store = Store::open(&dir.path().join("db.duckdb"), "replica", 100).expect("store");
        let service = MemoryService::new(store, vec![7; 32]);
        let mut allowed = auth();
        allowed.allowed_namespaces.insert("private".into());
        let saved = service
            .save(
                &allowed,
                SaveInput {
                    content: "private project note".into(),
                    namespace: Some("private".into()),
                    title: None,
                    kind: None,
                    tags: None,
                    confidence: None,
                    importance: None,
                    provenance: None,
                    expires_at: None,
                    client_request_id: None,
                },
            )
            .expect("save private memory");

        let updated = service
            .update(
                &allowed,
                UpdateInput {
                    memory_id: saved.memory.id.clone(),
                    expected_revision: 1,
                    content: Some("updated private project note".into()),
                    title: None,
                    kind: None,
                    tags: None,
                    confidence: None,
                    importance: None,
                    expires_at: None,
                },
            )
            .expect("update private memory");
        assert_eq!(updated.memory.namespace, "private");
        assert_eq!(updated.memory.revision, 2);

        assert!(matches!(
            service.update(
                &auth(),
                UpdateInput {
                    memory_id: saved.memory.id.clone(),
                    expected_revision: 2,
                    content: Some("attempted disclosure".into()),
                    title: None,
                    kind: None,
                    tags: None,
                    confidence: None,
                    importance: None,
                    expires_at: None,
                }
            ),
            Err(AppError::NotFound)
        ));
        assert!(
            service
                .get(
                    &auth(),
                    GetInput {
                        memory_id: saved.memory.id,
                        namespace: Some("private".into()),
                    },
                )
                .expect("private get response")
                .memory
                .is_none()
        );
    }
}
