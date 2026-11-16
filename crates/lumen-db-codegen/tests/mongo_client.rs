//! SPEC-GEN-001 on MongoDB and SPEC-MONGO-001 AC-002: the client generated for
//! `tests/mongo.prisma` is current, compiles, and works against a real MongoDB (when
//! `LUMEN_TEST_MONGO_URL` is set): ObjectIds, composite types, client-side defaults, includes and
//! transactions.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::too_many_lines,
    reason = "tests: a failure should stop the test; each live test is one scenario"
)]

#[rustfmt::skip]
#[path = "generated/mongo.rs"]
mod db;

use db::{Address, Client, Phone, account, session};
use lumen_orm::Set;

#[test]
fn the_generated_mongo_client_is_current() {
    let text = include_str!("mongo.prisma");
    let (schema, d) = lumen_schema::check(&lumen_schema::SourceFile::new("mongo.prisma", text));
    assert!(!d.has_errors());
    assert_eq!(
        lumen_db_codegen::generate(&schema, "mongo.prisma").unwrap(),
        include_str!("generated/mongo.rs"),
        "tests/generated/mongo.rs is out of date: regenerate it with the gen example"
    );
}

static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

async fn client() -> Option<Client> {
    let url = std::env::var("LUMEN_TEST_MONGO_URL").ok()?;
    let name = format!(
        "lumen_genm_{}_{}",
        std::process::id(),
        N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    );
    let client = Client::connect(&url, &name).await.unwrap();
    client.db().database().drop().await.unwrap();
    let (schema, _) = lumen_schema::check(&lumen_schema::SourceFile::new(
        "mongo.prisma",
        include_str!("mongo.prisma"),
    ));
    lumen_migrate::mongo::push(client.db().database(), &schema)
        .await
        .unwrap();
    Some(client)
}

fn new_account(email: &str) -> account::Create {
    account::Create {
        id: None,
        email: email.into(),
        address: None,
        phones: None,
        created_at: None,
    }
}

#[tokio::test]
async fn ac_002_documents_round_trip_with_object_ids_and_composite_types() {
    let Some(client) = client().await else {
        eprintln!("skipped: LUMEN_TEST_MONGO_URL is not set");
        return;
    };
    let ana = client
        .account()
        .create(account::Create {
            address: Some(Address {
                street: "1 Main".into(),
                city: "Hanoi".into(),
                zip: None,
            }),
            phones: Some(vec![Phone {
                number: "123".into(),
                kind: "mobile".into(),
            }]),
            ..new_account("ana@example.com")
        })
        .await
        .unwrap();
    assert_eq!(ana.address.as_ref().map(|a| a.city.as_str()), Some("Hanoi"));
    assert_eq!(ana.phones[0].number, "123");
    assert!(
        ana.created_at.timestamp_millis() > 0,
        "now() is filled by the client"
    );
    client
        .account()
        .create(new_account("bo@example.com"))
        .await
        .unwrap();

    let found = client
        .account()
        .find_unique(account::UniqueWhere::Id(ana.id))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(found.email, "ana@example.com");
    let both = client
        .account()
        .find_many()
        .filter(account::email::ends_with("@example.com"))
        .order_by(account::email::desc())
        .exec()
        .await
        .unwrap();
    assert_eq!(
        both.iter().map(|a| a.email.as_str()).collect::<Vec<_>>(),
        ["bo@example.com", "ana@example.com"]
    );

    let changed = client
        .account()
        .update(
            account::UniqueWhere::Email("ana@example.com".into()),
            account::Update {
                email: Set::To("ana@new.dev".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        (changed.id, changed.email.as_str()),
        (ana.id, "ana@new.dev"),
        "found first, so changing the unique key works"
    );

    let e = client
        .account()
        .create(new_account("bo@example.com"))
        .await
        .unwrap_err();
    assert_eq!(e.code, "P2002");
    assert_eq!(client.account().count(None).await.unwrap(), 2);
    let gone = client
        .account()
        .delete(account::UniqueWhere::Email("bo@example.com".into()))
        .await
        .unwrap();
    assert_eq!(gone.email, "bo@example.com");
    assert_eq!(
        client
            .account()
            .delete(account::UniqueWhere::Email("bo@example.com".into()))
            .await
            .unwrap_err()
            .code,
        "P2025"
    );
    client.db().database().drop().await.unwrap();
}

#[tokio::test]
async fn includes_and_transactions_on_mongo() {
    let Some(client) = client().await else { return };
    let ana = client
        .account()
        .create(new_account("ana@example.com"))
        .await
        .unwrap();
    let bo = client
        .account()
        .create(new_account("bo@example.com"))
        .await
        .unwrap();
    for (who, token) in [(ana.id, "t1"), (ana.id, "t2"), (bo.id, "t3")] {
        client
            .session()
            .create(session::Create {
                id: None,
                account_id: who,
                token: token.into(),
            })
            .await
            .unwrap();
    }
    let before = client.db().statements();
    let accounts = client
        .account()
        .find_many()
        .order_by(account::email::asc())
        .include_sessions()
        .order_by(session::token::desc())
        .exec()
        .await
        .unwrap();
    assert_eq!(
        client.db().statements() - before,
        2,
        "accounts, then all their sessions"
    );
    assert_eq!(
        accounts[0]
            .sessions
            .iter()
            .map(|s| s.token.as_str())
            .collect::<Vec<_>>(),
        ["t2", "t1"]
    );
    // SPEC-QUERY-001 FR-004: skip and take inside an include page each account's sessions
    let paged = client
        .account()
        .find_many()
        .order_by(account::email::asc())
        .include_sessions()
        .order_by(session::token::desc())
        .skip(1)
        .take(1)
        .exec()
        .await
        .unwrap();
    assert_eq!(
        paged[0]
            .sessions
            .iter()
            .map(|s| s.token.as_str())
            .collect::<Vec<_>>(),
        ["t1"]
    );
    let sessions = client
        .session()
        .find_many()
        .include_account()
        .exec()
        .await
        .unwrap();
    assert!(
        sessions
            .iter()
            .all(|s| s.account.id == s.session.account_id),
        "a required to-one is not an Option"
    );

    let result: lumen_orm::Result<()> = client
        .db()
        .transaction(async |tx| {
            account::on(&mut *tx)
                .create(new_account("cy@example.com"))
                .await?;
            account::on(&mut *tx)
                .create(new_account("ana@example.com"))
                .await?;
            Ok(())
        })
        .await;
    assert_eq!(result.unwrap_err().code, "P2002");
    assert!(
        client
            .account()
            .find_unique(account::UniqueWhere::Email("cy@example.com".into()))
            .await
            .unwrap()
            .is_none(),
        "rolled back"
    );
    client.db().database().drop().await.unwrap();
}

/// SPEC-QUERY-001 FR-002 on MongoDB through the generated client.
#[tokio::test]
async fn ac_fr_002_relation_filters_on_mongo() {
    let Some(client) = client().await else { return };
    let mut ids = Vec::new();
    for email in ["ana@example.com", "bo@example.com", "cy@example.com"] {
        ids.push(
            client
                .account()
                .create(new_account(email))
                .await
                .unwrap()
                .id,
        );
    }
    for (who, token) in [(0, "t1"), (0, "x2"), (1, "t3")] {
        let s = session::Create {
            id: None,
            account_id: ids[who],
            token: token.into(),
        };
        client.session().create(s).await.unwrap();
    }
    let emails = async |f: lumen_orm::Filter| {
        let found = client
            .account()
            .find_many()
            .filter(f)
            .order_by(account::email::asc());
        let found = found.exec().await.unwrap();
        found.into_iter().map(|a| a.email).collect::<Vec<_>>()
    };
    let t = || session::token::starts_with_insensitive("T");
    assert_eq!(
        emails(account::sessions::some(t())).await,
        ["ana@example.com", "bo@example.com"]
    );
    assert_eq!(
        emails(account::sessions::every(t())).await,
        ["bo@example.com", "cy@example.com"],
        "every is true for an account without sessions"
    );
    assert_eq!(
        emails(account::sessions::none(t())).await,
        ["cy@example.com"]
    );
    assert_eq!(
        emails(account::sessions::some(session::account::is(
            account::email::ends_with_insensitive("BO@EXAMPLE.COM")
        )))
        .await,
        ["bo@example.com"],
        "nested"
    );
    assert_eq!(
        client
            .session()
            .count(Some(session::account::is_not(account::email::equals(
                "ana@example.com"
            ))))
            .await
            .unwrap(),
        1
    );
    client.db().database().drop().await.unwrap();
}
