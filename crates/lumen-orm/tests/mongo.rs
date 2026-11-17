//! SPEC-MONGO-001 AC-001: the query layer on a real MongoDB gives the same answers as on Postgres
//! (the 48 filter cases in `common`, relation filters and many-to-many included, on the same data as
//! `tests/postgres.rs`). Runs when `LUMEN_TEST_MONGO_URL` is set.

#![allow(clippy::unwrap_used, reason = "tests: a failure should stop the test")]

mod common;

use std::sync::atomic::{AtomicUsize, Ordering};

use lumen_orm::mongo::{Find, MongoDb};
use lumen_orm::{Filter, Order, Related, Table, Value, col};
use mongodb::bson::{Decimal128, doc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, PartialEq)]
struct Item {
    id: i32,
    name: String,
    note: Option<String>,
    price: Decimal128,
    role: String,
    tags: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct Id {
    id: i32,
}

async fn setup() -> Option<MongoDb> {
    static N: AtomicUsize = AtomicUsize::new(0);
    let url = std::env::var("LUMEN_TEST_MONGO_URL").ok()?;
    let name = format!(
        "lumen_mongo_{}_{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    );
    let db = MongoDb::connect(&url, &name).await.unwrap();
    db.database().drop().await.unwrap();
    let items = db.database().collection::<mongodb::bson::Document>("Item");
    items
        .create_index(
            mongodb::IndexModel::builder()
                .keys(doc! { "name": 1 })
                .options(
                    mongodb::options::IndexOptions::builder()
                        .unique(true)
                        .build(),
                )
                .build(),
        )
        .await
        .unwrap();
    let price = |s: &str| s.parse::<Decimal128>().unwrap();
    let rows = vec![
        Item {
            id: 1,
            name: "Lamp".into(),
            note: Some("warm light".into()),
            price: price("12.50"),
            role: "READER".into(),
            tags: vec!["home".into()],
        },
        Item {
            id: 2,
            name: "lamp shade".into(),
            note: None,
            price: price("5.00"),
            role: "WRITER".into(),
            tags: vec!["home".into(), "deco".into()],
        },
        Item {
            id: 3,
            name: "Desk".into(),
            note: Some("100%_oak".into()),
            price: price("250.00"),
            role: "ADMIN".into(),
            tags: vec![],
        },
        Item {
            id: 4,
            name: "Chair".into(),
            note: Some("WARM seat".into()),
            price: price("80.00"),
            role: "WRITER".into(),
            tags: vec!["office".into()],
        },
    ];
    db.insert_many("Item", &rows).await.unwrap();
    Some(db)
}

async fn ids(db: &MongoDb, filter: Filter) -> Vec<i32> {
    let find = Find {
        collection: "Item".into(),
        filter: Some(filter),
        order: vec![("id".into(), Order::Asc)],
        fields: vec!["id".into()],
        ..Find::default()
    };
    db.find::<Id>(&find)
        .await
        .unwrap()
        .into_iter()
        .map(|r| r.id)
        .collect()
}

/// Parts, parents and tags for the shared cases (`common`). Items without a parent have no
/// `parentId` field. Tags are kept as id lists on both sides (`Item.tagIds`, `Tag.itemIds`), as a
/// many-to-many relation is on MongoDB; Chair has no `tagIds` field and "unused" an empty list.
async fn seed_relations(db: &MongoDb) {
    let parts: Vec<_> = common::PARTS
        .iter()
        .map(|(id, item, n)| doc! { "id": id, "itemId": item, "name": n })
        .collect();
    db.insert_many("Part", &parts).await.unwrap();
    for (child, parent) in common::PARENTS {
        db.update_many(
            "Item",
            Some(&col("id").equals(child)),
            vec![("parentId".into(), parent.into())],
        )
        .await
        .unwrap();
    }
    let tags: Vec<_> = common::TAGS
        .iter()
        .map(|(id, n)| doc! { "id": id, "name": n, "itemIds": common::items_of(*id) })
        .collect();
    db.insert_many("Tag", &tags).await.unwrap();
    for item in 1..=3 {
        db.database()
            .collection::<mongodb::bson::Document>("Item")
            .update_one(
                doc! { "id": item },
                doc! { "$set": { "tagIds": common::tags_of(item) } },
            )
            .await
            .unwrap();
    }
}

fn tags() -> Related {
    Related::id_list(Table::new("Tag"), "tagIds", "id")
}

fn items() -> Related {
    Related::id_list(Table::new("Item"), "itemIds", "id")
}

#[tokio::test]
async fn ac_001_every_filter_returns_the_same_rows_as_on_postgres() {
    let Some(db) = setup().await else {
        eprintln!("skipped: LUMEN_TEST_MONGO_URL is not set");
        return;
    };
    seed_relations(&db).await;
    let cases: Vec<_> = common::field_cases(|r| Value::Text(r.to_owned()))
        .into_iter()
        .chain(common::relation_cases())
        .chain(common::many_to_many_cases(&tags, &items))
        .collect();
    assert_eq!(cases.len(), 48);
    for (what, filter, want) in cases {
        assert_eq!(ids(&db, filter).await, want, "{what}");
    }
    db.database().drop().await.unwrap();
}

#[tokio::test]
async fn ordering_paging_counts_writes_and_unique_errors() {
    let Some(db) = setup().await else { return };
    let find = Find {
        collection: "Item".into(),
        order: vec![("price".into(), Order::Desc)],
        limit: Some(2),
        skip: Some(1),
        fields: vec!["id".into()],
        ..Find::default()
    };
    let page: Vec<i32> = db
        .find::<Id>(&find)
        .await
        .unwrap()
        .into_iter()
        .map(|r| r.id)
        .collect();
    assert_eq!(page, [4, 1]);
    assert_eq!(
        db.count("Item", Some(&col("role").equals("WRITER")))
            .await
            .unwrap(),
        2
    );
    assert_eq!(
        db.update_many(
            "Item",
            Some(&col("role").equals("WRITER")),
            vec![("note".into(), Value::Null)]
        )
        .await
        .unwrap(),
        2
    );
    assert_eq!(ids(&db, col("note").is_null()).await, [2, 4]);
    assert_eq!(
        db.delete_many("Item", Some(&col("id").equals(3)))
            .await
            .unwrap(),
        1
    );
    let again = Item {
        id: 9,
        name: "Lamp".into(),
        note: None,
        price: "1".parse().unwrap(),
        role: "READER".into(),
        tags: vec![],
    };
    let e = db.insert_many("Item", &[again]).await.unwrap_err();
    assert_eq!(e.code, "P2002");
    db.database().drop().await.unwrap();
}

#[tokio::test]
async fn connect_url_uses_the_database_in_the_url() {
    let bad = MongoDb::connect_url("mongodb://localhost:27017")
        .await
        .unwrap_err();
    assert_eq!(bad.code, "P1013");
    let Ok(server) = std::env::var("LUMEN_TEST_MONGO_URL") else {
        eprintln!("skipped: LUMEN_TEST_MONGO_URL is not set");
        return;
    };
    let (base, query) = server.split_once('?').unwrap_or((server.as_str(), ""));
    let url = format!("{}/lumen_url?{query}", base.trim_end_matches('/'));
    let db = MongoDb::connect_url(&url).await.unwrap();
    assert_eq!(db.database().name(), "lumen_url");
}

#[tokio::test]
async fn ac_fr_002_relation_filters_in_counts_writes_and_transactions() {
    let Some(db) = setup().await else { return };
    seed_relations(&db).await;
    let parts = || Related::new(Table::new("Part"), &[("itemId", "id")]);
    let bulb = || parts().some(col("name").equals("bulb"));
    assert_eq!(db.count("Item", Some(&bulb())).await.unwrap(), 2);
    let before = db.statements();
    assert_eq!(
        db.update_many("Item", Some(&bulb()), vec![("note".into(), "lit".into())])
            .await
            .unwrap(),
        2
    );
    assert_eq!(
        db.statements() - before,
        2,
        "one statement for the relation, one for the update"
    );
    let in_tx = db
        .transaction(async |tx| {
            let lit = lumen_orm::mongo::MongoRun::count(tx, "Item", Some(&bulb())).await?;
            let gone = lumen_orm::mongo::MongoRun::delete_many(
                tx,
                "Part",
                Some(
                    &Related::new(Table::new("Item"), &[("id", "itemId")])
                        .is(col("name").equals("Chair")),
                ),
            )
            .await?;
            Ok((lit, gone))
        })
        .await
        .unwrap();
    assert_eq!(in_tx, (2, 1));
    assert_eq!(ids(&db, parts().none(Filter::And(vec![]))).await, [3, 4]);
    let many = Related::through(Table::new("Tag"), Table::new("_ItemToTag"), &[], &[]);
    assert_eq!(
        db.count("Item", Some(&many.some(Filter::And(vec![]))))
            .await
            .unwrap_err()
            .code,
        "P2026",
        "MongoDB has no join tables"
    );
    db.database().drop().await.unwrap();
}

#[tokio::test]
async fn many_to_many_filters_skip_dangling_ids_and_run_in_transactions() {
    let Some(db) = setup().await else { return };
    seed_relations(&db).await;
    let items_coll = db.database().collection::<mongodb::bson::Document>("Item");
    // Desk keeps wood and gains an id no tag has; Chair only has one
    items_coll
        .update_one(doc! { "id": 3 }, doc! { "$push": { "tagIds": 99 } })
        .await
        .unwrap();
    items_coll
        .update_one(doc! { "id": 4 }, doc! { "$set": { "tagIds": [99] } })
        .await
        .unwrap();
    let any = || Filter::And(vec![]);
    assert_eq!(ids(&db, tags().some(any())).await, [1, 2, 3]);
    assert_eq!(ids(&db, tags().none(any())).await, [4]);
    assert_eq!(
        ids(&db, tags().every(col("name").equals("wood"))).await,
        [3, 4]
    );

    let light = || tags().some(col("name").equals("light"));
    let before = db.statements();
    assert_eq!(db.count("Item", Some(&light())).await.unwrap(), 2);
    assert_eq!(
        db.statements() - before,
        2,
        "one statement for the relation, one for the count"
    );
    let in_tx = db
        .transaction(async |tx| {
            let lit = lumen_orm::mongo::MongoRun::update_many(
                tx,
                "Item",
                Some(&light()),
                vec![("note".into(), "lit".into())],
            )
            .await?;
            let gone =
                lumen_orm::mongo::MongoRun::delete_many(tx, "Tag", Some(&items().none(any())))
                    .await?;
            Ok((lit, gone))
        })
        .await
        .unwrap();
    assert_eq!(in_tx, (2, 1), "two lit items, the unused tag deleted");
    assert_eq!(ids(&db, col("note").equals("lit")).await, [1, 2]);
    db.database().drop().await.unwrap();
}

#[tokio::test]
async fn a_duplicate_names_the_fields_of_the_unique_index() {
    let Some(db) = setup().await else { return };
    let items = db.database().collection::<mongodb::bson::Document>("Item");
    items
        .create_index(
            mongodb::IndexModel::builder()
                .keys(doc! { "role": 1, "price": -1 })
                .options(
                    mongodb::options::IndexOptions::builder()
                        .unique(true)
                        .name("Item_role_price_key".to_owned())
                        .build(),
                )
                .build(),
        )
        .await
        .unwrap();
    let db = db.with_constraints(std::collections::HashMap::from([
        ("Item.name_1".to_owned(), vec!["name".to_owned()]),
        (
            "Item.Item_role_price_key".to_owned(),
            vec!["role".to_owned(), "price".to_owned()],
        ),
    ]));
    let item = |id: i32, name: &str, role: &str| Item {
        id,
        name: name.into(),
        note: None,
        price: "12.50".parse().unwrap(),
        role: role.into(),
        tags: vec![],
    };

    let e = db
        .insert_many("Item", &[item(9, "Lamp", "ADMIN")])
        .await
        .unwrap_err();
    assert_eq!(e.code, "P2002");
    assert_eq!(e.fields, ["name"]);
    assert_eq!(e.constraint.as_deref(), Some("name_1"));
    assert_eq!(
        e.to_string(),
        "P2002: a record with the same name already exists"
    );

    let e = db
        .insert_many("Item", &[item(9, "Torch", "READER")])
        .await
        .unwrap_err();
    assert_eq!(e.fields, ["role", "price"], "a compound unique index");

    let e = db
        .update_many(
            "Item",
            Some(&col("id").equals(2)),
            vec![("name".into(), "Desk".into())],
        )
        .await
        .unwrap_err();
    assert_eq!(
        (e.code, e.fields.clone()),
        ("P2002", vec!["name".to_owned()])
    );

    let first: mongodb::bson::Document = items.find_one(doc! {}).await.unwrap().unwrap();
    let again = doc! { "_id": first.get("_id").unwrap().clone(), "id": 10, "name": "Again" };
    let e = db
        .transaction(async |tx| lumen_orm::mongo::MongoRun::insert_many(tx, "Item", &[again]).await)
        .await
        .unwrap_err();
    assert_eq!(
        e.fields,
        ["_id"],
        "the built-in _id index, in a transaction"
    );

    let unknown = MongoDb::new(db.database().clone());
    let e = unknown
        .insert_many("Item", &[item(9, "Lamp", "ADMIN")])
        .await
        .unwrap_err();
    assert!(e.fields.is_empty(), "an index the schema did not name");
    assert_eq!(
        e.to_string(),
        "P2002: a record with the same unique value (index name_1) already exists"
    );
    db.database().drop().await.unwrap();
}
