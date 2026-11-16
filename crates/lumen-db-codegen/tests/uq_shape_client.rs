//! SPEC-CLI-001 FR-004: a lumen client on a collection another app's generic repository writes
//! (uq-core's `MongoRepo`: string UUID v7 `_id`, camelCase, BSON dates, `orgId`, soft delete via
//! `deletedAt`). Both sides read what the other wrote, so an app can move one collection at a
//! time. The live part runs when `LUMEN_TEST_MONGO_URL` is set.

#![allow(clippy::unwrap_used, reason = "tests: a failure should stop the test")]

#[rustfmt::skip]
#[path = "generated/uq_shape.rs"]
mod db;

use db::{Client, note};
use lumen_orm::mongo::DateTime;
use mongodb::bson::{Bson, Document, doc};

#[test]
fn the_generated_uq_shape_client_is_current() {
    let text = include_str!("uq_shape.prisma");
    let (schema, d) = lumen_schema::check(&lumen_schema::SourceFile::new("uq_shape.prisma", text));
    assert!(!d.has_errors());
    assert_eq!(
        lumen_db_codegen::generate(&schema, "uq_shape.prisma").unwrap(),
        include_str!("generated/uq_shape.rs"),
        "tests/generated/uq_shape.rs is out of date: regenerate it with the gen example"
    );
}

#[tokio::test]
async fn both_sides_read_what_the_other_wrote() {
    let Ok(url) = std::env::var("LUMEN_TEST_MONGO_URL") else {
        eprintln!("skipped: LUMEN_TEST_MONGO_URL is not set");
        return;
    };
    let client = Client::connect(&url, &format!("lumen_uq_{}", std::process::id()))
        .await
        .unwrap();
    let raw = client.db().database().collection::<Document>("notes");
    raw.drop().await.unwrap();

    // what MongoRepo::create writes, and a soft-deleted one
    let now = DateTime::now();
    let old_id = "0192f0c4-7a1e-7cc0-8000-000000000001";
    raw.insert_one(doc! {
        "_id": old_id, "orgId": "org1", "ownerId": "u1", "title": "from uq-core",
        "createdAt": now, "updatedAt": now,
    })
    .await
    .unwrap();
    raw.insert_one(doc! {
        "_id": "0192f0c4-7a1e-7cc0-8000-000000000002", "orgId": "org1", "ownerId": "u1",
        "title": "deleted", "createdAt": now, "updatedAt": now, "deletedAt": now,
    })
    .await
    .unwrap();

    // lumen reads them, and skips the deleted one the way MongoRepo does
    let live = client
        .note()
        .find_many()
        .filter(note::org_id::equals("org1"))
        .filter(note::deleted_at::is_null())
        .exec()
        .await
        .unwrap();
    assert_eq!(live.len(), 1);
    assert_eq!(live[0].id, old_id);
    assert_eq!(live[0].title, "from uq-core");
    assert_eq!(live[0].body, None);

    // lumen writes one; it has the shape MongoRepo reads back
    let made = client
        .note()
        .create(note::Create {
            id: None,
            org_id: Some("org1".into()),
            owner_id: Some("u2".into()),
            title: "from lumen".into(),
            body: None,
            created_at: None,
            updated_at: None,
            deleted_at: None,
        })
        .await
        .unwrap();
    let stored = raw
        .find_one(doc! { "_id": &made.id })
        .await
        .unwrap()
        .unwrap();
    let id = lumen_orm::uuid::Uuid::parse_str(stored.get_str("_id").unwrap()).unwrap();
    assert_eq!(id.get_version_num(), 7, "a UUID v7, like MongoRepo makes");
    assert!(matches!(stored.get("createdAt"), Some(Bson::DateTime(_))));
    assert!(matches!(stored.get("updatedAt"), Some(Bson::DateTime(_))));
    assert_eq!(stored.get_str("orgId").unwrap(), "org1");
    assert!(
        stored.get("deletedAt").is_none(),
        "absent, as MongoRepo writes it"
    );
    assert!(stored.get("body").is_none());

    client.db().database().drop().await.unwrap();
}
