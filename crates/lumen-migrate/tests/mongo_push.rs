//! SPEC-MONGO-001 FR-002: `db push` on a real MongoDB creates collections and Prisma-named indexes,
//! rebuilds the schema's indexes whose definition changed, reports indexes the schema does not
//! have without dropping them (hand-made ones included), and never drops a collection. Runs when
//! `LUMEN_TEST_MONGO_URL` is set.

#![allow(clippy::unwrap_used, reason = "tests: a failure should stop the test")]

use lumen_migrate::mongo::{self, MongoPush, MongoStep};
use lumen_schema::{SourceFile, check};
use mongodb::IndexModel;
use mongodb::bson::{Document, doc};
use mongodb::options::IndexOptions;

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
    let pushed = mongo::push(&db, &corpus).await.unwrap();
    assert_eq!(pushed.not_in_schema, []);
    assert_eq!(
        pushed.steps,
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
        MongoPush::default(),
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
    let pushed = mongo::push(&db, &schema(&changed)).await.unwrap();
    assert_eq!(
        pushed.steps,
        [
            MongoStep::CreateCollection("Gone".into()),
            MongoStep::DropIndex("Session".into(), "Session_accountId_idx".into()),
            MongoStep::CreateIndex("Session".into(), "Session_accountId_idx".into()),
        ],
        "the schema's own index is rebuilt; the one it no longer has is not dropped"
    );
    assert_eq!(
        names(&pushed),
        ["Session.Session_token_key"],
        "the removed @unique is reported"
    );
    assert!(
        index_names(&db, "Session")
            .await
            .contains(&"Session_token_key".to_owned()),
        "and still in the database"
    );
    // a model removed from the schema keeps its collection (and data)
    let pushed = mongo::push(&db, &corpus).await.unwrap();
    assert!(
        !pushed
            .steps
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

fn names(pushed: &MongoPush) -> Vec<String> {
    pushed
        .not_in_schema
        .iter()
        .map(|e| format!("{}.{}", e.collection, e.index.name))
        .collect()
}

async fn index_names(db: &mongodb::Database, collection: &str) -> Vec<String> {
    db.collection::<Document>(collection)
        .list_index_names()
        .await
        .unwrap()
}

#[tokio::test]
async fn ac_fr_002_db_push_reports_hand_made_indexes_and_never_drops_them() {
    let Ok(url) = std::env::var("LUMEN_TEST_MONGO_URL") else {
        eprintln!("skipped: LUMEN_TEST_MONGO_URL is not set");
        return;
    };
    let client = mongodb::Client::with_uri_str(&url).await.unwrap();
    let db = client.database(&format!("lumen_push_hand_{}", std::process::id()));
    db.drop().await.unwrap();
    let corpus = schema(include_str!("../../lumen-schema/tests/corpus/mongo.prisma"));
    mongo::push(&db, &corpus).await.unwrap();

    // someone adds an index for a report query, outside the schema
    let hand_made = |keys: Document, name: &str| {
        IndexModel::builder()
            .keys(keys)
            .options(IndexOptions::builder().name(name.to_owned()).build())
            .build()
    };
    let accounts = db.collection::<Document>("Account");
    accounts
        .create_index(hand_made(doc! { "createdAt": -1 }, "by_newest"))
        .await
        .unwrap();
    let pushed = mongo::push(&db, &corpus).await.unwrap();
    assert_eq!(pushed.steps, []);
    assert_eq!(names(&pushed), ["Account.by_newest"]);
    assert_eq!(
        pushed.not_in_schema[0].index.keys,
        [("createdAt".into(), -1)]
    );
    assert!(
        index_names(&db, "Account")
            .await
            .contains(&"by_newest".to_owned())
    );

    // the schema now asks for an index on the same keys under its own name: MongoDB would refuse
    // it, so push stops before running anything and says what to do
    let wanted = include_str!("../../lumen-schema/tests/corpus/mongo.prisma").replace(
        "  createdAt DateTime  @default(now())\n}",
        "  createdAt DateTime  @default(now())\n\n  @@index([createdAt(sort: Desc)])\n}",
    );
    let wanted = schema(&wanted);
    let err = mongo::push(&db, &wanted).await.unwrap_err();
    assert!(
        err.message.contains("Account_createdAt_idx") && err.message.contains("by_newest"),
        "{}",
        err.message
    );
    assert!(
        index_names(&db, "Account")
            .await
            .contains(&"by_newest".to_owned()),
        "nothing was dropped"
    );
    // dropping it by hand, as the message says, lets the push go ahead
    accounts.drop_index("by_newest").await.unwrap();
    let pushed = mongo::push(&db, &wanted).await.unwrap();
    assert_eq!(
        pushed.steps,
        [MongoStep::CreateIndex(
            "Account".into(),
            "Account_createdAt_idx".into()
        )]
    );
    db.drop().await.unwrap();
}
