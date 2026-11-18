use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use nova_core::{compilation::AssetScope, intake::IntakeStore, *};
use nova_review::ReviewServer;
use serde_json::Value;
use tower::ServiceExt;

fn server(authority: &str) -> (tempfile::TempDir, ReviewServer) {
    let d = tempfile::tempdir().unwrap();
    let db = d.path().join("legacy.sqlite");
    let key = d.path().join("vault.key");
    let k = VaultKey::generate().unwrap();
    k.write_new_file(&key).unwrap();
    drop(Vault::create(&db, k.clone()).unwrap());
    let owner = d.path().join("owner.key");
    VaultKey::generate()
        .unwrap()
        .write_new_file(&owner)
        .unwrap();
    let intake = d.path().join("intake.sqlite");
    drop(
        IntakeStore::create(
            &intake,
            k,
            AssetScope {
                owner_id: "owner:fixture".into(),
                space: "test".into(),
            },
        )
        .unwrap(),
    );
    let server = ReviewServer::new(db, key, authority.into(), true)
        .with_owner("owner:fixture".into(), "test".into())
        .with_owner_token(&owner)
        .unwrap()
        .with_intake_store(&intake)
        .unwrap();
    (d, server)
}

#[tokio::test]
async fn runtime_instance_is_public_binding_and_never_an_owner_credential() {
    let (_d, s) = server("127.0.0.1:31444");
    let instance = "0123456789abcdef0123456789abcdef";
    assert!(
        s.clone()
            .with_local_runtime_instance(instance.into())
            .is_err()
    );
    let s = s.with_native_workspace().unwrap();
    for invalid in [
        "",
        "ABCDEF0123456789abcdef0123456789",
        "0123456789abcdef0123456789abcdeg",
        "../0123456789abcdef0123456789abc",
    ] {
        assert!(
            s.clone()
                .with_local_runtime_instance(invalid.into())
                .is_err()
        );
    }
    let r = s
        .with_local_runtime_instance(instance.into())
        .unwrap()
        .router();
    let response = r
        .clone()
        .oneshot(
            Request::builder()
                .uri("/console/session")
                .header("Host", "127.0.0.1:31444")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let value: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 65536).await.unwrap()).unwrap();
    assert_eq!(value["local_runtime_instance"], instance);
    assert_eq!(value["requires_owner_token"], true);
    let response = r
        .oneshot(
            Request::builder()
                .uri("/console/api/owner/intake/configuration")
                .header("Host", "127.0.0.1:31444")
                .header("X-Nova-Review", "owner-review")
                .header("Authorization", format!("Bearer {instance}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[test]
fn runtime_instance_requires_literal_loopback_authority() {
    for authority in [
        "example.invalid:31444",
        "192.0.2.1:31444",
        "localhost:31444",
    ] {
        let (_d, s) = server(authority);
        assert!(
            s.with_native_workspace()
                .unwrap()
                .with_local_runtime_instance("0123456789abcdef0123456789abcdef".into())
                .is_err()
        );
    }
}
