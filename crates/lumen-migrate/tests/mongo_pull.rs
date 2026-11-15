//! SPEC-MONGO-001 FR-005 and AC-002: `db pull` suggests a schema for a uq-core-style collection
//! (camelCase fields, ObjectId ids, nested documents, optional and mixed fields, a unique index),
//! and the suggestion checks clean. Runs when `LUMEN_TEST_MONGO_URL` is set.

#![allow(clippy::unwrap_used, reason = "tests: a failure should stop the test")]

use mongodb::bson::{doc, oid::ObjectId};

#[tokio::test]
async fn ac_002_pull_suggests_a_schema_that_checks() {
    let Ok(url) = std::env::var("LUMEN_TEST_MONGO_URL") else {
        eprintln!("skipped: LUMEN_TEST_MONGO_URL is not set");
        return;
    };
    let client = mongodb::Client::with_uri_str(&url).await.unwrap();
    let db = client.database(&format!("lumen_pull_{}", std::process::id()));
    db.drop().await.unwrap();
    let jobs = db.collection::<mongodb::bson::Document>("jobs");
    jobs.insert_many([
        doc! { "_id": ObjectId::new(), "title": "Unity port", "budgetUsd": 800, "createdAt": mongodb::bson::DateTime::now(), "client": { "country": "JP", "rating": 4.9 }, "tags": ["unity", "c#"], "score": 72 },
        doc! { "_id": ObjectId::new(), "title": "Rust API", "budgetUsd": 1200, "createdAt": mongodb::bson::DateTime::now(), "client": { "country": "VN", "rating": 4.5 }, "tags": ["rust"], "score": "n/a", "notes": "repeat client" },
        doc! { "_id": ObjectId::new(), "title": "Svelte site", "budgetUsd": 400, "createdAt": mongodb::bson::DateTime::now(), "client": { "country": "US", "rating": 5.0 }, "tags": [], "score": 55, "account id": ObjectId::new() },
    ])
    .await
    .unwrap();
    jobs.create_index(
        mongodb::IndexModel::builder()
            .keys(doc! { "title": 1 })
            .options(
                mongodb::options::IndexOptions::builder()
                    .unique(true)
                    .build(),
            )
            .build(),
    )
    .await
    .unwrap();
    jobs.create_index(
        mongodb::IndexModel::builder()
            .keys(doc! { "budgetUsd": -1, "createdAt": 1 })
            .build(),
    )
    .await
    .unwrap();

    let schema = lumen_migrate::mongo_pull::pull(&db).await.unwrap();
    db.drop().await.unwrap();
    for line in [
        "model Jobs {",
        "id         String     @id @default(auto()) @map(\"_id\") @db.ObjectId",
        "title      String     @unique",
        "budgetUsd  Int",
        "createdAt  DateTime",
        "client     JobsClient",
        "tags       String[]",
        "// lumen: `score` holds several types (Int 66%, String 33%); chose Int",
        "score      Int",
        "notes      String?",
        "account_id String?    @map(\"account id\") @db.ObjectId",
        "@@index([budgetUsd(sort: Desc), createdAt])",
        "@@map(\"jobs\")",
        "type JobsClient {",
        "country String",
        "rating  Float",
    ] {
        assert!(schema.contains(line), "missing `{line}` in:\n{schema}");
    }
    let file = lumen_schema::SourceFile::new("schema.prisma", &schema);
    let (_, d) = lumen_schema::check(&file);
    assert!(!d.has_errors(), "{}\n{schema}", d.render(&file));
}
