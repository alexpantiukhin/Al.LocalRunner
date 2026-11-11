mod common;

use common::*;
use lr_abstractions::fakes::{ManualClock, MemoryBrokerStore};
use lr_abstractions::{BrokerStore, MachineKind, StoreError};
use lr_net::{
    BrokerIdentity, CodeError, CodeRequest, Fingerprint, JoinCode, Registry, RegistryError,
    DEFAULT_CODE_TTL_MIN,
};
use lr_proto::ProtocolErrorCode;
use std::sync::Arc;

fn request(kind: MachineKind, uses: u32, ttl_min: u64) -> CodeRequest {
    CodeRequest {
        kind,
        name: None,
        label: None,
        uses,
        ttl_min,
    }
}

#[tokio::test]
async fn code_is_single_use_and_expires() {
    let b = broker().await;
    let code = b
        .registry
        .issue(request(MachineKind::Runner, 1, 60), T0)
        .await
        .unwrap()
        .reveal();
    let first = key();
    b.registry
        .redeem(
            join_request(&code, "light", &first),
            &first.public_key(),
            at_min(1),
        )
        .await
        .expect("first join works");
    let second = key();
    let reuse = b
        .registry
        .redeem(
            join_request(&code, "heavy", &second),
            &second.public_key(),
            at_min(2),
        )
        .await
        .unwrap_err();
    assert_eq!(reuse, RegistryError::Store(StoreError::CodeUsedUp));
    assert_eq!(reuse.to_protocol().code, ProtocolErrorCode::JoinCodeUsedUp);

    let late = b
        .registry
        .issue(request(MachineKind::Runner, 1, 60), T0)
        .await
        .unwrap()
        .reveal();
    let expired = b
        .registry
        .redeem(
            join_request(&late, "heavy", &second),
            &second.public_key(),
            at_min(60),
        )
        .await
        .unwrap_err();
    assert_eq!(expired, RegistryError::Store(StoreError::CodeExpired));
    let refusal = expired.to_protocol();
    assert_eq!(refusal.code, ProtocolErrorCode::JoinCodeExpired);
    assert_eq!(refusal.message, "join code expired");
}

#[tokio::test]
async fn code_lives_ttl_minutes_from_issue() {
    let b = broker().await;
    b.registry
        .issue(request(MachineKind::Hub, 1, 15), T0)
        .await
        .unwrap();
    let rows = b.store.join_codes().await.unwrap();
    assert_eq!(rows[0].expires_at, at_min(15));
    assert_eq!(rows[0].issued_at, T0);
    assert_eq!(DEFAULT_CODE_TTL_MIN, 60);
    let k = key();
    let code = b
        .registry
        .issue(request(MachineKind::Runner, 1, 15), T0)
        .await
        .unwrap()
        .reveal();
    let just_before = lr_core::Timestamp(at_min(15).0 - 1);
    b.registry
        .redeem(join_request(&code, "r1", &k), &k.public_key(), just_before)
        .await
        .expect("still valid one millisecond before expiry");
}

#[tokio::test]
async fn zero_uses_or_zero_ttl_refused() {
    let b = broker().await;
    for req in [
        request(MachineKind::Runner, 0, 60),
        request(MachineKind::Runner, 1, 0),
    ] {
        assert!(matches!(
            b.registry.issue(req, T0).await,
            Err(RegistryError::Invalid(_))
        ));
    }
}

#[tokio::test]
async fn only_the_hash_of_a_code_is_stored() {
    let b = broker().await;
    let code = b
        .registry
        .issue(request(MachineKind::Runner, 1, 60), T0)
        .await
        .unwrap();
    let text = code.reveal();
    let rows = b.store.join_codes().await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].code_hash, code.hash());
    let secret = text.split('.').nth(2).unwrap();
    assert!(!format!("{:?}", rows[0]).contains(secret));
    assert!(
        !format!("{code:?}").contains(secret),
        "Debug hides the secret"
    );
}

#[tokio::test]
async fn code_carries_the_broker_fingerprint() {
    let b = broker().await;
    let code = b.issue(MachineKind::Runner, 1).await;
    let parsed = JoinCode::parse(&code).unwrap();
    assert_eq!(parsed.fingerprint(), b.identity.fingerprint());
    assert_eq!(parsed.reveal(), code);
    assert_eq!(JoinCode::parse(&format!("  {code}\n")).unwrap(), parsed);
}

#[tokio::test]
async fn codes_are_unique_and_high_entropy() {
    let fp = BrokerIdentity::generate().unwrap().fingerprint();
    let a = JoinCode::generate(fp).unwrap();
    let b = JoinCode::generate(fp).unwrap();
    assert_ne!(a.reveal(), b.reveal());
    assert_ne!(a.hash(), b.hash());
    let secret = a.reveal().split('.').nth(2).unwrap().to_string();
    assert_eq!(secret.len(), 43, "32 random bytes in base64url");
}

#[test]
fn tampered_code_refused() {
    let fp = Fingerprint::from_bytes([7; 32]);
    let code = JoinCode::generate(fp).unwrap().reveal();
    let parts: Vec<&str> = code.split('.').collect();
    let other_fp = "A".repeat(43);
    let altered_fp = format!("{}.{}.{}.{}", parts[0], other_fp, parts[2], parts[3]);
    assert_eq!(JoinCode::parse(&altered_fp), Err(CodeError::Damaged));
    let mut secret: Vec<char> = parts[2].chars().collect();
    secret[5] = if secret[5] == 'A' { 'B' } else { 'A' };
    let secret: String = secret.into_iter().collect();
    let typo = format!("{}.{}.{}.{}", parts[0], parts[1], secret, parts[3]);
    assert_eq!(JoinCode::parse(&typo), Err(CodeError::Damaged));
    for bad in [
        "",
        "lrj1",
        "lrj2.a.b.c",
        &code[..code.len() - 3],
        "lrj1.!!.b.c",
    ] {
        assert!(
            matches!(JoinCode::parse(bad), Err(CodeError::Malformed(_))),
            "{bad:?}"
        );
    }
}

#[tokio::test]
async fn code_of_another_broker_refused() {
    let a = broker().await;
    let b = broker().await;
    let code = a.issue(MachineKind::Runner, 1).await;
    let k = key();
    let err = b
        .registry
        .redeem(join_request(&code, "r1", &k), &k.public_key(), at_min(1))
        .await
        .unwrap_err();
    assert_eq!(err, RegistryError::ForeignCode);
    assert_eq!(err.to_protocol().code, ProtocolErrorCode::JoinCodeUnknown);
}

#[tokio::test]
async fn code_for_n_runners_admits_n_joins() {
    let b = broker().await;
    let code = b.issue(MachineKind::Runner, 3).await;
    for i in 1..=3 {
        let k = key();
        b.registry
            .redeem(
                join_request(&code, &format!("heavy-{i}"), &k),
                &k.public_key(),
                at_min(1),
            )
            .await
            .unwrap_or_else(|e| panic!("join {i}: {e}"));
    }
    let k = key();
    let fourth = b
        .registry
        .redeem(
            join_request(&code, "heavy-4", &k),
            &k.public_key(),
            at_min(1),
        )
        .await
        .unwrap_err();
    assert_eq!(fourth, RegistryError::Store(StoreError::CodeUsedUp));
}

#[tokio::test]
async fn fixed_name_and_label_come_from_the_code() {
    let b = broker().await;
    let code = b
        .registry
        .issue(
            CodeRequest {
                kind: MachineKind::Hub,
                name: Some("ci-box".into()),
                label: Some("ci".into()),
                uses: 1,
                ttl_min: 60,
            },
            T0,
        )
        .await
        .unwrap()
        .reveal();
    let k = key();
    let mut req = join_request(&code, "laptop", &k);
    req.kind = MachineKind::Hub;
    let wrong = b
        .registry
        .redeem(req.clone(), &k.public_key(), at_min(1))
        .await
        .unwrap_err();
    assert_eq!(wrong, RegistryError::Store(StoreError::CodeNameMismatch));
    req.name = "ci-box".into();
    let row = b
        .registry
        .redeem(req, &k.public_key(), at_min(1))
        .await
        .unwrap();
    assert_eq!(row.label, "ci");
    assert_eq!(row.public_key, k.public_key());
}

#[tokio::test]
async fn runner_code_refuses_a_hub() {
    let b = broker().await;
    let code = b.issue(MachineKind::Runner, 1).await;
    let k = key();
    let mut req = join_request(&code, "laptop", &k);
    req.kind = MachineKind::Hub;
    let err = b
        .registry
        .redeem(req, &k.public_key(), at_min(1))
        .await
        .unwrap_err();
    assert_eq!(err, RegistryError::Store(StoreError::CodeKindMismatch));
}

#[tokio::test]
async fn hub_code_refuses_a_runner() {
    let b = broker().await;
    let code = b.issue(MachineKind::Hub, 1).await;
    let k = key();
    let mut req = join_request(&code, "r1", &k);
    req.kind = MachineKind::Runner;
    let err = b
        .registry
        .redeem(req, &k.public_key(), at_min(1))
        .await
        .unwrap_err();
    assert_eq!(err, RegistryError::Store(StoreError::CodeKindMismatch));
    let mut as_hub = join_request(&code, "laptop", &k);
    as_hub.kind = MachineKind::Hub;
    let row = b
        .registry
        .redeem(as_hub, &k.public_key(), at_min(1))
        .await
        .expect("the refused attempt did not spend the code");
    assert_eq!((row.name.as_str(), row.kind), ("laptop", MachineKind::Hub));
}

#[tokio::test]
async fn joining_with_the_broker_machine_id_refused() {
    let b = broker().await;
    let code = b.issue(MachineKind::Runner, 1).await;
    let k = key();
    let mut req = join_request(&code, "r1", &k);
    req.machine_id = BROKER_MACHINE_ID.into();
    let err = b
        .registry
        .redeem(req, &k.public_key(), at_min(1))
        .await
        .unwrap_err();
    assert_eq!(err, RegistryError::MachineIdClash);
    assert_eq!(err.to_protocol().code, ProtocolErrorCode::MachineIdClash);
    let fresh = b.issue(MachineKind::Runner, 1).await;
    b.registry
        .redeem(join_request(&fresh, "r1", &k), &k.public_key(), at_min(1))
        .await
        .expect("the refused join spent no use of anything");
}

#[tokio::test]
async fn join_key_must_be_the_proven_key() {
    let b = broker().await;
    let code = b.issue(MachineKind::Runner, 1).await;
    let (claimed, proven) = (key(), key());
    let err = b
        .registry
        .redeem(
            join_request(&code, "r1", &claimed),
            &proven.public_key(),
            at_min(1),
        )
        .await
        .unwrap_err();
    assert_eq!(err, RegistryError::KeyNotProven);
    assert_eq!(err.to_protocol().code, ProtocolErrorCode::Unauthorized);
}

#[tokio::test]
async fn taken_name_refused_and_bad_names_refused() {
    let b = broker().await;
    b.register(&key(), "light").await;
    let code = b.issue(MachineKind::Runner, 1).await;
    let k = key();
    let err = b
        .registry
        .redeem(join_request(&code, "light", &k), &k.public_key(), at_min(1))
        .await
        .unwrap_err();
    assert_eq!(err.to_protocol().code, ProtocolErrorCode::NameTaken);
    for bad in ["", "local:alex", "a b", &"x".repeat(65)] {
        let err = b
            .registry
            .redeem(join_request(&code, bad, &k), &k.public_key(), at_min(1))
            .await
            .unwrap_err();
        assert!(matches!(err, RegistryError::Invalid(_)), "{bad:?}: {err:?}");
    }
}

#[tokio::test]
async fn revoke_marks_the_record_and_keeps_others() {
    let b = broker().await;
    let (a, c) = (key(), key());
    b.register(&a, "light").await;
    b.register(&c, "heavy").await;
    b.registry.revoke("light", at_min(5)).await.unwrap();
    let machines = b.registry.machines().await.unwrap();
    let light = machines.iter().find(|m| m.name == "light").unwrap();
    let heavy = machines.iter().find(|m| m.name == "heavy").unwrap();
    assert_eq!(light.revoked_at, Some(at_min(5)));
    assert_eq!(heavy.revoked_at, None);
}

#[tokio::test]
async fn listener_needed_only_with_live_code_or_machine() {
    let identity = BrokerIdentity::generate().unwrap();
    let store: Arc<dyn BrokerStore> = Arc::new(MemoryBrokerStore::new());
    let clock = Arc::new(ManualClock::new(T0));
    let registry = Registry::new(store, clock, BROKER_MACHINE_ID, identity.fingerprint());
    assert!(!registry.should_listen(T0).await.unwrap());
    let code = registry
        .issue(request(MachineKind::Runner, 1, 60), T0)
        .await
        .unwrap()
        .reveal();
    assert!(registry.should_listen(at_min(59)).await.unwrap());
    assert!(
        !registry.should_listen(at_min(60)).await.unwrap(),
        "expired"
    );
    let k = key();
    registry
        .redeem(join_request(&code, "r1", &k), &k.public_key(), at_min(1))
        .await
        .unwrap();
    assert!(
        registry.should_listen(at_min(90)).await.unwrap(),
        "a machine"
    );
    registry.revoke("r1", at_min(91)).await.unwrap();
    assert!(!registry.should_listen(at_min(92)).await.unwrap());
}
