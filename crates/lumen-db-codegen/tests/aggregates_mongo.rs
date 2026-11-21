//! `aggregate()` and `group_by()` in a generated MongoDB client, against a real replica set
//! (when `LUMEN_TEST_MONGO_URL` is set). The results follow Postgres: a count of 0 and no other
//! values over no documents, and no sum when no document has a value.

#![allow(
    clippy::unwrap_used,
    clippy::too_many_lines,
    reason = "tests: a failure should stop the test; each live test is one scenario"
)]

#[rustfmt::skip]
#[path = "generated/stats_mongo.rs"]
#[allow(dead_code)]
mod db;

use db::{Client, sale};
use lumen_orm::mongo::Decimal128;

#[test]
fn the_generated_stats_client_is_current() {
    let text = include_str!("stats_mongo.prisma");
    let (schema, d) =
        lumen_schema::check(&lumen_schema::SourceFile::new("stats_mongo.prisma", text));
    assert!(!d.has_errors());
    assert_eq!(
        lumen_db_codegen::generate(&schema, "stats_mongo.prisma").unwrap(),
        include_str!("generated/stats_mongo.rs"),
        "tests/generated/stats_mongo.rs is out of date: regenerate it with the gen example"
    );
}

fn dec(s: &str) -> Decimal128 {
    s.parse().unwrap()
}

async fn sale(client: &Client, region: &str, units: i32, amount: &str, score: Option<f64>) {
    client
        .sale()
        .create(sale::Create {
            id: None,
            region: region.into(),
            units,
            amount: dec(amount),
            score,
            note: score.map(|s| format!("scored {s}")),
            at: None,
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn aggregates_and_groups_on_mongo() {
    let Ok(url) = std::env::var("LUMEN_TEST_MONGO_URL") else {
        return;
    };
    let name = format!("lumen_aggm_{}", std::process::id());
    let client = Client::connect(&url, &name).await.unwrap();
    client.db().database().drop().await.unwrap();
    sale(&client, "north", 10, "1.10", Some(2.0)).await;
    sale(&client, "north", 30, "2.20", None).await;
    sale(&client, "north", 50, "3.30", Some(4.0)).await;
    sale(&client, "south", 5, "0.40", None).await;

    let before = client.db().statements();
    let stats = client
        .sale()
        .aggregate()
        .count()
        .value(sale::units::sum())
        .value(sale::units::avg())
        .value(sale::units::max())
        .value(sale::region::min())
        .value(sale::amount::sum())
        .value(sale::note::count())
        .exec()
        .await
        .unwrap();
    assert_eq!(client.db().statements() - before, 1);
    assert_eq!(stats.count, Some(4));
    assert_eq!(stats.sum.units, Some(95));
    assert_eq!(stats.avg.units, Some(23.75));
    assert_eq!(stats.max.units, Some(50));
    assert_eq!(stats.min.region.as_deref(), Some("north"));
    assert_eq!(
        stats.sum.amount.map(|d| d.to_string()).as_deref(),
        Some("7.00")
    );
    assert_eq!(stats.count_of.note, Some(2));
    assert_eq!(stats.sum.score, None, "not asked for");

    // nothing matches: a count of 0 and no sum, as on Postgres
    let none = client
        .sale()
        .aggregate()
        .filter(sale::units::gt(1000))
        .count()
        .value(sale::units::sum())
        .value(sale::note::count())
        .exec()
        .await
        .unwrap();
    assert_eq!(none.count, Some(0));
    assert_eq!(none.count_of.note, Some(0));
    assert_eq!(none.sum.units, None);

    // take and skip pick the documents before aggregating
    let top_two = client
        .sale()
        .aggregate()
        .order_by(sale::units::desc())
        .take(2)
        .value(sale::units::sum())
        .exec()
        .await
        .unwrap();
    assert_eq!(top_two.sum.units, Some(80));

    // groups: a sum over no values is None, not 0
    let groups = client
        .sale()
        .group_by([sale::region::COLUMN])
        .count()
        .value(sale::score::sum())
        .having(sale::units::sum().gte(5))
        .order_by_value(sale::units::sum().desc())
        .exec()
        .await
        .unwrap();
    assert_eq!(groups.len(), 2);
    assert_eq!(groups[0].key.region.as_deref(), Some("north"));
    assert_eq!(groups[0].key.units, None);
    assert_eq!(groups[0].values.count, Some(3));
    assert_eq!(groups[0].values.sum.units, Some(90));
    assert_eq!(groups[0].values.sum.score, Some(6.0));
    assert_eq!(groups[1].key.region.as_deref(), Some("south"));
    assert_eq!(groups[1].values.sum.score, None);

    let busy = client
        .sale()
        .group_by([sale::region::COLUMN])
        .having(sale::aggregates::count().gt(1))
        .exec()
        .await
        .unwrap();
    assert_eq!(busy.len(), 1);
    assert_eq!(busy[0].key.region.as_deref(), Some("north"));

    // paging the groups, ordered by the group field
    let page = client
        .sale()
        .group_by([sale::region::COLUMN])
        .count()
        .order_by(sale::region::asc())
        .skip(1)
        .take(1)
        .exec()
        .await
        .unwrap();
    assert_eq!(page.len(), 1);
    assert_eq!(page[0].key.region.as_deref(), Some("south"));

    // inside a transaction
    let in_tx = client
        .db()
        .transaction(async |tx| {
            sale::on(&mut *tx)
                .aggregate()
                .value(sale::units::min())
                .value(sale::at::max())
                .exec()
                .await
        })
        .await
        .unwrap();
    assert_eq!(in_tx.min.units, Some(5));
    assert!(in_tx.max.at.is_some());

    client.db().database().drop().await.unwrap();
}
