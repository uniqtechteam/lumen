//! `aggregate()` and `group_by()` in the generated client, against a real Postgres (when
//! `LUMEN_TEST_POSTGRES_URL` is set). The type checks run in `tests/ui` with the other
//! compile-fail cases.

#![allow(
    clippy::unwrap_used,
    clippy::too_many_lines,
    reason = "tests: a failure should stop the test; each live test is one scenario"
)]

#[rustfmt::skip]
#[path = "generated/blog.rs"]
#[allow(dead_code)]
mod db;

#[rustfmt::skip]
#[path = "generated/shop.rs"]
#[allow(dead_code)]
mod shop_db;

use db::{Client, post, user};
use lumen_orm::rust_decimal::Decimal;
use lumen_orm::sqlx::{AssertSqlSafe, PgPool};

/// A fresh database with `schema` migrated in: the admin pool, a pool on it, and its name.
async fn database(file: &str, schema: &str) -> Option<(PgPool, PgPool, String)> {
    let url = std::env::var("LUMEN_TEST_POSTGRES_URL").ok()?;
    let admin = PgPool::connect(&url).await.unwrap();
    let name = format!(
        "lumen_agg_{}_{}",
        std::process::id(),
        file.trim_end_matches(".prisma")
    );
    for sql in [
        format!("drop database if exists {name} with (force)"),
        format!("create database {name}"),
    ] {
        lumen_orm::sqlx::raw_sql(AssertSqlSafe(sql))
            .execute(&admin)
            .await
            .unwrap();
    }
    let base = url.rsplit_once('/').map_or(url.as_str(), |(b, _)| b);
    let pool = PgPool::connect(&format!("{base}/{name}")).await.unwrap();
    let (schema, _) = lumen_schema::check(&lumen_schema::SourceFile::new(file, schema));
    let steps = lumen_migrate::diff(
        &lumen_migrate::Shape::default(),
        &lumen_migrate::calculate(&schema),
        &lumen_migrate::Renames::default(),
    );
    lumen_orm::sqlx::raw_sql(AssertSqlSafe(lumen_migrate::postgres::script(&steps, &[])))
        .execute(&pool)
        .await
        .unwrap();
    Some((admin, pool, name))
}

async fn drop_database(admin: &PgPool, pool: PgPool, name: &str) {
    pool.close().await;
    lumen_orm::sqlx::raw_sql(AssertSqlSafe(format!(
        "drop database if exists {name} with (force)"
    )))
    .execute(admin)
    .await
    .unwrap();
}

async fn client() -> Option<(PgPool, Client, String)> {
    let (admin, pool, name) = database("blog.prisma", include_str!("blog.prisma")).await?;
    Some((admin, Client::new(lumen_orm::Db::from_pool(pool)), name))
}

async fn user(client: &Client, email: &str) -> i32 {
    client
        .user()
        .create(user::Create {
            id: None,
            email: email.into(),
            name: None,
            role: None,
            created_at: None,
            updated_at: None,
        })
        .await
        .unwrap()
        .id
}

async fn post(client: &Client, author_id: i32, title: &str, views: i64, published: bool) {
    client
        .post()
        .create(post::Create {
            id: None,
            title: title.into(),
            body: published.then(|| format!("{title} body")),
            published: Some(published),
            author_id,
            views: Some(views),
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn aggregates_and_groups() {
    let Some((admin, client, name)) = client().await else {
        return;
    };
    let ana = user(&client, "ana@example.com").await;
    let bo = user(&client, "bo@example.com").await;
    post(&client, ana, "a1", 10, true).await;
    post(&client, ana, "a2", 30, false).await;
    post(&client, ana, "a3", 50, true).await;
    post(&client, bo, "b1", 5, true).await;

    // over the records that match, in one statement
    let before = client.db().statements();
    let stats = client
        .post()
        .aggregate()
        .filter(post::published::equals(true))
        .count()
        .value(post::views::sum())
        .value(post::views::avg())
        .value(post::views::max())
        .value(post::title::min())
        .value(post::body::count())
        .exec()
        .await
        .unwrap();
    assert_eq!(client.db().statements() - before, 1);
    assert_eq!(stats.count, Some(3));
    assert_eq!(stats.sum.views, Some(65));
    assert!((stats.avg.views.unwrap() - 65.0 / 3.0).abs() < 1e-9);
    assert_eq!(stats.max.views, Some(50));
    assert_eq!(stats.min.title.as_deref(), Some("a1"));
    assert_eq!(stats.count_of.body, Some(3));
    // values not asked for stay None
    assert_eq!(stats.sum.id, None);
    assert_eq!(stats.min.views, None);

    // nothing matches: a count of 0, no sum
    let none = client
        .post()
        .aggregate()
        .filter(post::views::gt(1000))
        .count()
        .value(post::views::sum())
        .exec()
        .await
        .unwrap();
    assert_eq!(none.count, Some(0));
    assert_eq!(none.sum.views, None);

    // no value asked for counts
    let all = client.post().aggregate().exec().await.unwrap();
    assert_eq!(all.count, Some(4));

    // take and skip pick the records before aggregating
    let top_two = client
        .post()
        .aggregate()
        .order_by(post::views::desc())
        .take(2)
        .value(post::views::sum())
        .exec()
        .await
        .unwrap();
    assert_eq!(top_two.sum.views, Some(80));

    // groups, with a condition on an aggregate and ordering by one
    let before = client.db().statements();
    let groups = client
        .post()
        .group_by([post::author_id::COLUMN])
        .count()
        .having(post::views::sum().gte(5))
        .order_by_value(post::views::sum().desc())
        .exec()
        .await
        .unwrap();
    assert_eq!(client.db().statements() - before, 1);
    assert_eq!(groups.len(), 2);
    assert_eq!(groups[0].key.author_id, Some(ana));
    assert_eq!(groups[0].key.title, None);
    assert_eq!(groups[0].values.count, Some(3));
    assert_eq!(groups[0].values.sum.views, Some(90));
    assert_eq!(groups[1].key.author_id, Some(bo));
    assert_eq!(groups[1].values.sum.views, Some(5));

    let busy = client
        .post()
        .group_by([post::author_id::COLUMN])
        .having(
            post::aggregates::count()
                .gt(1)
                .and(post::views::max().gte(50)),
        )
        .exec()
        .await
        .unwrap();
    assert_eq!(busy.len(), 1);
    assert_eq!(busy[0].key.author_id, Some(ana));
    assert_eq!(busy[0].values.count, Some(3));
    assert_eq!(busy[0].values.max.views, Some(50));

    // a condition on a group column, ordering by it, and paging the groups
    let page = client
        .post()
        .group_by([post::published::COLUMN])
        .count()
        .having(post::published::equals(true).or(post::published::equals(false)))
        .order_by(post::published::asc())
        .skip(1)
        .take(1)
        .exec()
        .await
        .unwrap();
    assert_eq!(page.len(), 1);
    assert_eq!(page[0].key.published, Some(true));
    assert_eq!(page[0].values.count, Some(3));

    // dates: the newest user
    let users = client
        .user()
        .aggregate()
        .value(user::created_at::max())
        .value(user::created_at::min())
        .exec()
        .await
        .unwrap();
    assert!(users.max.created_at >= users.min.created_at);
    assert!(users.max.created_at.is_some());

    drop_database(&admin, client.db().pool().clone(), &name).await;
}

#[tokio::test]
async fn decimal_sums_and_averages_keep_every_digit() {
    let Some((admin, pool, name)) = database("shop.prisma", include_str!("shop.prisma")).await
    else {
        return;
    };
    let client = shop_db::Client::new(lumen_orm::Db::from_pool(pool));
    for (sku, price) in [("a", "0.10"), ("b", "0.20"), ("c", "1000000.05")] {
        client
            .product()
            .create(shop_db::product::Create {
                id: None,
                sku: sku.into(),
                price: price.parse().unwrap(),
                data: None,
                image: None,
                archived: None,
            })
            .await
            .unwrap();
    }
    let stats = client
        .product()
        .aggregate()
        .value(shop_db::product::price::sum())
        .value(shop_db::product::price::avg())
        .value(shop_db::product::price::min())
        .value(shop_db::product::sku::max())
        .exec()
        .await
        .unwrap();
    assert_eq!(
        stats.sum.price,
        Some("1000000.35".parse::<Decimal>().unwrap())
    );
    assert_eq!(
        stats.avg.price.unwrap().round_dp(4),
        "333333.4500".parse::<Decimal>().unwrap()
    );
    assert_eq!(stats.min.price, Some("0.10".parse::<Decimal>().unwrap()));
    assert_eq!(stats.max.sku.as_deref(), Some("c"));
    drop_database(&admin, client.db().pool().clone(), &name).await;
}
