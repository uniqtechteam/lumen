//! SPEC-MONGO-001 AC-001: the query layer on a real MongoDB gives the same answers as on Postgres
//! (the same 23 filter cases and data as `tests/postgres.rs`). Runs when `LUMEN_TEST_MONGO_URL` is
//! set.

#![allow(clippy::unwrap_used, reason = "tests: a failure should stop the test")]

use std::sync::atomic::{AtomicUsize, Ordering};

use lumen_orm::mongo::{Find, MongoDb};
use lumen_orm::{Filter, Order, Value, col};
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

#[tokio::test]
async fn ac_001_every_filter_returns_the_same_rows_as_on_postgres() {
    let Some(db) = setup().await else {
        eprintln!("skipped: LUMEN_TEST_MONGO_URL is not set");
        return;
    };
    let dec = |s: &str| Value::Decimal(s.to_owned());
    let cases: Vec<(&str, Filter, Vec<i32>)> = vec![
        ("equals", col("name").equals("Lamp"), vec![1]),
        (
            "equals, ignoring case",
            col("name").equals_insensitive("lamp"),
            vec![1],
        ),
        (
            "not equals keeps NULLs, like Prisma's `not`",
            col("note").not_equals("warm light"),
            vec![2, 3, 4],
        ),
        ("in", col("id").is_in([1, 3]), vec![1, 3]),
        ("not in", col("id").not_in([1, 3]), vec![2, 4]),
        (
            "in an empty list matches nothing",
            col("id").is_in(Vec::<i32>::new()),
            vec![],
        ),
        ("lt on decimals", col("price").lt(dec("12.50")), vec![2]),
        ("lte", col("price").lte(dec("12.50")), vec![1, 2]),
        ("gt", col("price").gt(dec("80")), vec![3]),
        ("gte", col("price").gte(dec("80")), vec![3, 4]),
        ("contains", col("name").contains("amp"), vec![1, 2]),
        (
            "contains, ignoring case",
            col("note").contains_insensitive("warm"),
            vec![1, 4],
        ),
        (
            "contains escapes % and _ (and regex characters)",
            col("note").contains("0%_o"),
            vec![3],
        ),
        ("starts with", col("name").starts_with("lamp"), vec![2]),
        ("ends with", col("name").ends_with("k"), vec![3]),
        ("is null", col("note").is_null(), vec![2]),
        ("is not null", col("note").is_not_null(), vec![1, 3, 4]),
        (
            "enum values are strings",
            col("role").equals("WRITER"),
            vec![2, 4],
        ),
        (
            "and",
            col("role").equals("WRITER").and(col("note").is_null()),
            vec![2],
        ),
        (
            "or",
            col("id").equals(1).or(col("id").equals(4)),
            vec![1, 4],
        ),
        ("not", !col("id").lt(3), vec![3, 4]),
        (
            "an empty and is true",
            Filter::And(vec![]),
            vec![1, 2, 3, 4],
        ),
        ("an empty or is false", Filter::Or(vec![]), vec![]),
    ];
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
