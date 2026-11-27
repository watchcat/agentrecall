// AR-001 characterization: passing tests below confirm a known bug, not healthy replication.
// Replace the rejection assertions with causal-progress assertions when AR-001 is fixed.
use super::{SyncRequest, receive_exchange};
use crate::auth::AuthContext;
use crate::domain::{AppError, Memory, MutationEvent, MutationOperation, now_rfc3339};
use crate::store::{Store, UpdatePatch};
use std::collections::HashSet;
use uuid::Uuid;

const NAMESPACE: &str = "ar001-replication";
const OWNER_ID: &str = "ar001-owner";

struct Fixture {
    _dir: tempfile::TempDir,
    a: Store,
    b: Store,
    auth_a: AuthContext,
    auth_b: AuthContext,
    memory_id: String,
    a_branch: String,
    b_branch: String,
}

fn auth(client_id: &str) -> AuthContext {
    AuthContext {
        owner_id: OWNER_ID.into(),
        client_id: client_id.into(),
        scopes: HashSet::from(["memory:read".into(), "memory:write".into()]),
        allowed_namespaces: HashSet::from([NAMESPACE.into()]),
        token_fingerprint: format!("{client_id}-ar001"),
    }
}

fn memory(id: &str, content: &str, client_id: &str) -> Memory {
    let now = now_rfc3339();
    Memory {
        id: id.into(),
        namespace: NAMESPACE.into(),
        content: content.into(),
        title: None,
        kind: Default::default(),
        tags: vec![],
        confidence: 1.0,
        importance: 0.5,
        provenance: Default::default(),
        source_client: client_id.into(),
        created_at: now.clone(),
        updated_at: now,
        expires_at: None,
        revision: 1,
        deleted_at: None,
        version_event_id: None,
    }
}

fn save(store: &Store, ctx: &AuthContext, value: Memory) -> Memory {
    let fingerprint = format!("ar001-save-{}", value.id);
    store
        .save_record(ctx, value, None, &fingerprint)
        .expect("save memory")
        .0
}

fn patch(content: &str) -> UpdatePatch {
    UpdatePatch {
        content: Some(content.into()),
        title: None,
        kind: None,
        tags: None,
        confidence: None,
        importance: None,
        expires_at: None,
    }
}

fn update(store: &Store, ctx: &AuthContext, id: &str, content: &str) {
    store
        .update_record(ctx, id, NAMESPACE, 1, patch(content))
        .expect("update revision 1");
}

fn exchange(source: &Store, receiver: &Store) -> Result<(), AppError> {
    let request = SyncRequest {
        source_replica_id: source.replica_id().into(),
        remote_cursor: source
            .inbound_cursor(receiver.replica_id())
            .expect("inbound cursor"),
        events: source
            .pending_events(receiver.replica_id(), 128)
            .expect("pending events"),
    };
    let response = receive_exchange(receiver, request)?;
    source.apply_replication_response(
        receiver.replica_id(),
        response.accepted_through,
        &response.events,
    )?;
    Ok(())
}

fn pending_update_id(store: &Store, peer: &str) -> String {
    let pending = store.pending_events(peer, 128).expect("pending events");
    pending
        .iter()
        .find(|e| matches!(&e.operation, MutationOperation::Update))
        .expect("update event")
        .event_id
        .clone()
}

fn branch_ids(store: &Store) -> Vec<String> {
    assert_eq!(store.conflict_count().expect("conflict count"), 1);
    let conflicts = store.list_conflicts().expect("list conflicts");
    assert_eq!(conflicts.len(), 1);
    let conflict = &conflicts[0];
    assert_eq!(conflict["owner_id"], OWNER_ID);
    assert_eq!(conflict["namespace"], NAMESPACE);
    let branches: Vec<MutationEvent> = serde_json::from_value(conflict["branches"].clone())
        .expect("retained branches use the canonical MutationEvent contract");
    branches.into_iter().map(|event| event.event_id).collect()
}

fn assert_no_conflicts(store: &Store) {
    assert_eq!(store.conflict_count().expect("conflict count"), 0);
    assert!(store.list_conflicts().expect("list conflicts").is_empty());
}

fn setup() -> Fixture {
    let dir = tempfile::tempdir().expect("temp dir");
    let a = Store::open(&dir.path().join("a.duckdb"), "replica-a", 1024).expect("open A");
    let b = Store::open(&dir.path().join("b.duckdb"), "replica-b", 1024).expect("open B");
    let auth_a = auth("client-a");
    let auth_b = auth("client-b");
    let memory_id = Uuid::now_v7().to_string();
    let saved = save(&a, &auth_a, memory(&memory_id, "base", &auth_a.client_id));
    assert_eq!(saved.revision, 1);
    exchange(&a, &b).expect("replicate base to B");
    let mirrored = b
        .get_record(&auth_b, &memory_id, NAMESPACE)
        .expect("read base")
        .expect("base on B");
    assert_eq!(mirrored.revision, 1);

    update(&a, &auth_a, &memory_id, "branch-from-a");
    update(&b, &auth_b, &memory_id, "branch-from-b");
    let a_branch = pending_update_id(&a, b.replica_id());
    let b_branch = pending_update_id(&b, a.replica_id());
    exchange(&a, &b).expect("exchange A update");
    exchange(&b, &a).expect("exchange B update");
    let mut a_ids = branch_ids(&a);
    let mut b_ids = branch_ids(&b);
    a_ids.sort();
    b_ids.sort();
    assert_eq!(a_ids.len(), 2);
    assert_eq!(a_ids, b_ids);
    assert!(a_ids.contains(&a_branch));
    assert!(a_ids.contains(&b_branch));
    Fixture {
        _dir: dir,
        a,
        b,
        auth_a,
        auth_b,
        memory_id,
        a_branch,
        b_branch,
    }
}

#[derive(Clone)]
struct ExchangeAttempt {
    source_replica_id: String,
    remote_cursor: i64,
    events: Vec<MutationEvent>,
}
impl ExchangeAttempt {
    fn capture(source: &Store, peer: &str) -> Self {
        Self {
            source_replica_id: source.replica_id().into(),
            remote_cursor: source.inbound_cursor(peer).expect("inbound cursor"),
            events: source.pending_events(peer, 128).expect("pending events"),
        }
    }
    fn request(&self) -> SyncRequest {
        SyncRequest {
            source_replica_id: self.source_replica_id.clone(),
            remote_cursor: self.remote_cursor,
            events: self.events.clone(),
        }
    }
}

fn expect_unavailable(receiver: &Store, attempt: &ExchangeAttempt, label: &str) {
    let cursor = receiver
        .inbound_cursor(&attempt.source_replica_id)
        .expect("cursor before failed exchange");
    let queued_ids = |store: &Store| {
        store
            .pending_events(&attempt.source_replica_id, 128)
            .expect("queued events around failed exchange")
            .into_iter()
            .map(|event| event.event_id)
            .collect::<Vec<_>>()
    };
    let before_queue = queued_ids(receiver);
    match receive_exchange(receiver, attempt.request()) {
        Err(AppError::TemporarilyUnavailable) => {
            eprintln!(
                "ar001 {label}: TemporarilyUnavailable, receiver={} inbound_cursor={cursor}",
                receiver.replica_id()
            );
        }
        Err(other) => panic!("{label}: expected TemporarilyUnavailable, got {other:?}"),
        Ok(_) => panic!("{label}: expected TemporarilyUnavailable, got success"),
    }
    assert_eq!(
        receiver
            .inbound_cursor(&attempt.source_replica_id)
            .expect("cursor after failed exchange"),
        cursor
    );
    assert_eq!(
        queued_ids(receiver),
        before_queue,
        "failed exchange must not acknowledge queued events"
    );
}

fn later_only(attempt: &ExchangeAttempt) -> ExchangeAttempt {
    let later = attempt
        .events
        .iter()
        .find(|e| matches!(&e.operation, MutationOperation::Save))
        .expect("save event");
    ExchangeAttempt {
        events: vec![later.clone()],
        ..attempt.clone()
    }
}

fn expect_gap(receiver: &Store, attempt: &ExchangeAttempt, label: &str) {
    let later = &attempt.events[0];
    let cursor = receiver
        .inbound_cursor(&attempt.source_replica_id)
        .expect("peer cursor");
    assert_eq!(
        later.origin_seq,
        cursor + 2,
        "{label}: missing resolution gap"
    );
    expect_unavailable(receiver, attempt, label);
}

fn assert_pending_resolution_then_save(
    source: &Store,
    peer: &str,
    resolution: &MutationEvent,
    later: &Memory,
) {
    let pending = source.pending_events(peer, 128).expect("pending events");
    assert_eq!(pending.len(), 2);
    assert_eq!(pending[0].event_id, resolution.event_id);
    assert!(matches!(&pending[0].operation, MutationOperation::Resolve));
    assert_eq!(pending[0].origin_seq, resolution.origin_seq);
    assert!(matches!(&pending[1].operation, MutationOperation::Save));
    assert_eq!(pending[1].origin_seq, resolution.origin_seq + 1);
    assert_eq!(pending[1].memory_id, later.id);
}

fn characterize(f: &Fixture, a_to_b_first: bool) {
    let ar =
        f.a.resolve_conflict(OWNER_ID, &f.memory_id, &f.a_branch, &f.auth_a.client_id)
            .expect("A resolve");
    let br =
        f.b.resolve_conflict(OWNER_ID, &f.memory_id, &f.b_branch, &f.auth_b.client_id)
            .expect("B resolve");
    assert_ne!(ar.event_id, br.event_id);
    assert!(matches!(&ar.operation, MutationOperation::Resolve));
    assert!(matches!(&br.operation, MutationOperation::Resolve));
    assert_eq!(ar.base_event_id.as_deref(), Some(f.a_branch.as_str()));
    assert_eq!(br.base_event_id.as_deref(), Some(f.b_branch.as_str()));
    let resolved_a =
        f.a.get_record(&f.auth_a, &f.memory_id, NAMESPACE)
            .expect("read A resolution")
            .expect("A resolution remains live");
    let resolved_b =
        f.b.get_record(&f.auth_b, &f.memory_id, NAMESPACE)
            .expect("read B resolution")
            .expect("B resolution remains live");
    assert_eq!(resolved_a.content, "branch-from-a");
    assert_eq!(resolved_b.content, "branch-from-b");
    assert_eq!(resolved_a.revision, 3);
    assert_eq!(resolved_b.revision, 3);
    assert_no_conflicts(&f.a);
    assert_no_conflicts(&f.b);
    let al = save(
        &f.a,
        &f.auth_a,
        memory(
            &Uuid::now_v7().to_string(),
            "later-from-a",
            &f.auth_a.client_id,
        ),
    );
    let bl = save(
        &f.b,
        &f.auth_b,
        memory(
            &Uuid::now_v7().to_string(),
            "later-from-b",
            &f.auth_b.client_id,
        ),
    );
    let aa = ExchangeAttempt::capture(&f.a, f.b.replica_id());
    let ba = ExchangeAttempt::capture(&f.b, f.a.replica_id());
    assert_eq!(aa.events.len(), 2);
    assert_eq!(ba.events.len(), 2);
    let ac = f.a.inbound_cursor(f.b.replica_id()).expect("A cursor");
    let bc = f.b.inbound_cursor(f.a.replica_id()).expect("B cursor");
    assert_eq!(ac, br.origin_seq - 1);
    assert_eq!(bc, ar.origin_seq - 1);
    let order = if a_to_b_first {
        "a-to-b-first"
    } else {
        "b-to-a-first"
    };
    eprintln!(
        "ar001 order={order} seq a={}->{} b={}->{} cursor a<-b={} b<-a={}",
        ar.origin_seq, aa.events[1].origin_seq, br.origin_seq, ba.events[1].origin_seq, ac, bc
    );

    if a_to_b_first {
        expect_unavailable(&f.b, &aa, "initial A->B");
        expect_unavailable(&f.a, &ba, "initial B->A");
    } else {
        expect_unavailable(&f.a, &ba, "initial B->A");
        expect_unavailable(&f.b, &aa, "initial A->B");
    }
    for retry in 1..=2 {
        if a_to_b_first {
            expect_unavailable(&f.b, &aa, &format!("retry {retry} A->B"));
            expect_unavailable(&f.a, &ba, &format!("retry {retry} B->A"));
        } else {
            expect_unavailable(&f.a, &ba, &format!("retry {retry} B->A"));
            expect_unavailable(&f.b, &aa, &format!("retry {retry} A->B"));
        }
    }
    expect_gap(&f.b, &later_only(&aa), "A later event alone");
    expect_gap(&f.a, &later_only(&ba), "B later event alone");
    assert_eq!(
        f.a.inbound_cursor(f.b.replica_id())
            .expect("read replication cursor"),
        ac
    );
    assert_eq!(
        f.b.inbound_cursor(f.a.replica_id())
            .expect("read replication cursor"),
        bc
    );
    assert_pending_resolution_then_save(&f.a, f.b.replica_id(), &ar, &al);
    assert_pending_resolution_then_save(&f.b, f.a.replica_id(), &br, &bl);
    assert!(
        f.b.get_record(&f.auth_b, &al.id, NAMESPACE)
            .expect("read A later on B")
            .is_none()
    );
    assert!(
        f.a.get_record(&f.auth_a, &bl.id, NAMESPACE)
            .expect("read B later on A")
            .is_none()
    );
    eprintln!("ar001 order={order} retries=2 cursor a<-b={ac} b<-a={bc}");
}

#[test]
fn ar001_concurrent_resolutions_a_to_b_first() {
    let f = setup();
    characterize(&f, true);
}

#[test]
fn ar001_concurrent_resolutions_b_to_a_first() {
    let f = setup();
    characterize(&f, false);
}

#[test]
fn ar001_single_resolver_progresses() {
    let f = setup();
    assert_eq!(branch_ids(&f.b).len(), 2);
    f.a.resolve_conflict(OWNER_ID, &f.memory_id, &f.a_branch, &f.auth_a.client_id)
        .expect("A resolve only");
    assert_no_conflicts(&f.a);
    assert_eq!(f.b.conflict_count().expect("B conflicts"), 1);
    let ac =
        f.a.inbound_cursor(f.b.replica_id())
            .expect("read replication cursor");
    let bc =
        f.b.inbound_cursor(f.a.replica_id())
            .expect("read replication cursor");
    exchange(&f.a, &f.b).expect("B accepts A resolution");
    assert_no_conflicts(&f.b);
    let resolved =
        f.b.get_record(&f.auth_b, &f.memory_id, NAMESPACE)
            .expect("read resolved")
            .expect("resolved record");
    assert_eq!(resolved.content, "branch-from-a");
    let al = save(
        &f.a,
        &f.auth_a,
        memory(
            &Uuid::now_v7().to_string(),
            "control-later-from-a",
            &f.auth_a.client_id,
        ),
    );
    let bl = save(
        &f.b,
        &f.auth_b,
        memory(
            &Uuid::now_v7().to_string(),
            "control-later-from-b",
            &f.auth_b.client_id,
        ),
    );
    exchange(&f.a, &f.b).expect("replicate A later");
    exchange(&f.b, &f.a).expect("replicate B later");
    assert!(
        f.b.get_record(&f.auth_b, &al.id, NAMESPACE)
            .expect("read A later on B")
            .is_some()
    );
    assert!(
        f.a.get_record(&f.auth_a, &bl.id, NAMESPACE)
            .expect("read B later on A")
            .is_some()
    );
    assert!(
        f.a.inbound_cursor(f.b.replica_id())
            .expect("read replication cursor")
            > ac
    );
    assert!(
        f.b.inbound_cursor(f.a.replica_id())
            .expect("read replication cursor")
            > bc
    );
    eprintln!(
        "ar001 control cursor a<-b={} b<-a={}",
        f.a.inbound_cursor(f.b.replica_id())
            .expect("read replication cursor"),
        f.b.inbound_cursor(f.a.replica_id())
            .expect("read replication cursor")
    );
}
