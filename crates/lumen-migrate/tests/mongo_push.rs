//! SPEC-MONGO-001 FR-002: `db push` on a real MongoDB creates collections and Prisma-named indexes,
//! rebuilds changed indexes, drops removed ones, and never drops a collection. Runs when
//! `LUMEN_TEST_MONGO_URL` is set.

#![allow(clippy::unwrap_used, reason = "tests: a failure should stop the test")]

use lumen_migrate::mongo::{self, MongoStep};
use lumen_schema::{SourceFile, check};

fn schema(text: &str) -> lumen_schema::checked::Schema {
    let (s, d) = check(&SourceFile::new("schema.prisma", text));
    assert!(
        !d.has_errors(),
        "{}",
        d.render(&SourceFile::new("schema.prisma", text))
    );
    s
}

#[tokio::test]
async fn ac_fr_002_db_push_keeps_collections_and_indexes_in_line() {
    let Ok(url) = std::env::var("LUMEN_TEST_MONGO_URL") else {
        eprintln!("skipped: LUMEN_TEST_MONGO_URL is not set");
        return;
    };
    let client = mongodb::Client::with_uri_str(&url).await.unwrap();
    let db = client.database(&format!("lumen_push_{}", std::process::id()));
    db.drop().await.unwrap();

    let corpus = schema(include_str!("../../lumen-schema/tests/corpus/mongo.prisma"));
    let steps = mongo::push(&db, &corpus).await.unwrap();
    assert_eq!(
        steps,
        [
            MongoStep::CreateCollection("Account".into()),
            MongoStep::CreateCollection("Session".into()),
            MongoStep::CreateIndex("Account".into(), "Account_email_key".into()),
            MongoStep::CreateIndex("Session".into(), "Session_accountId_idx".into()),
            MongoStep::CreateIndex("Session".into(), "Session_token_key".into()),
        ]
    );
    assert_eq!(
        mongo::push(&db, &corpus).await.unwrap(),
        Vec::<MongoStep>::new(),
        "nothing to do the second time"
    );
    assert_eq!(
        mongo::describe(&db).await.unwrap(),
        mongo::calculate(&corpus),
        "the server matches the schema"
    );

    // the index on accountId becomes descending and the token is no longer unique
    let changed = include_str!("../../lumen-schema/tests/corpus/mongo.prisma")
        .replace("@@index([accountId])", "@@index([accountId(sort: Desc)])")
        .replace("token     String  @unique", "token     String")
        .replace("model Account {", "model Gone {\n  id String @id @default(auto()) @map(\"_id\") @db.ObjectId\n}\n\nmodel Account {");
    let steps = mongo::push(&db, &schema(&changed)).await.unwrap();
    assert_eq!(
        steps,
        [
            MongoStep::CreateCollection("Gone".into()),
            MongoStep::DropIndex("Session".into(), "Session_accountId_idx".into()),
            MongoStep::DropIndex("Session".into(), "Session_token_key".into()),
            MongoStep::CreateIndex("Session".into(), "Session_accountId_idx".into()),
        ]
    );
    // a model removed from the schema keeps its collection (and data)
    let steps = mongo::push(&db, &corpus).await.unwrap();
    assert!(
        !steps
            .iter()
            .any(|s| matches!(s, MongoStep::CreateCollection(c) if c == "Gone"))
    );
    assert!(
        db.list_collection_names()
            .await
            .unwrap()
            .contains(&"Gone".to_owned())
    );
    db.drop().await.unwrap();
}
