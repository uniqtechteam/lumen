//! SPEC-QUERY-001 FR-004..006 on a real Postgres: related rows for many parents in one query per
//! level, `take` per parent in SQL, composite keys, and transactions (commit, rollback,
//! savepoints, isolation). Runs when `LUMEN_TEST_POSTGRES_URL` is set.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "tests: a failure should stop the test"
)]

use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

use lumen_orm::load::{load_many, load_many_composite, load_one};
use lumen_orm::sqlx::{AssertSqlSafe, FromRow, PgPool};
use lumen_orm::{Db, Error, Insert, Isolation, Order, Run, Select, Table, Value, col};

const SCHEMA: &str = r#"
CREATE TABLE "User" ("id" SERIAL PRIMARY KEY, "name" TEXT NOT NULL);
CREATE TABLE "Profile" ("id" SERIAL PRIMARY KEY, "userId" INTEGER NOT NULL UNIQUE REFERENCES "User"("id"), "bio" TEXT NOT NULL);
CREATE TABLE "Post" ("id" SERIAL PRIMARY KEY, "authorId" INTEGER NOT NULL REFERENCES "User"("id"), "title" TEXT NOT NULL);
CREATE TABLE "Comment" ("id" SERIAL PRIMARY KEY, "postId" INTEGER NOT NULL REFERENCES "Post"("id"), "body" TEXT NOT NULL);
CREATE TABLE "Line" ("orderId" INTEGER NOT NULL, "n" INTEGER NOT NULL, PRIMARY KEY ("orderId", "n"));
CREATE TABLE "Shipment" ("id" SERIAL PRIMARY KEY, "orderId" INTEGER NOT NULL, "n" INTEGER NOT NULL, FOREIGN KEY ("orderId", "n") REFERENCES "Line"("orderId", "n"));
"#;

#[derive(Debug, Clone, FromRow)]
struct User {
    id: i32,
    name: String,
}

#[derive(Debug, Clone, FromRow)]
struct Profile {
    #[sqlx(rename = "userId")]
    user_id: i32,
    bio: String,
}

#[derive(Debug, Clone, FromRow)]
struct Post {
    id: i32,
    #[sqlx(rename = "authorId")]
    author_id: i32,
    title: String,
}

#[derive(Debug, Clone, FromRow)]
struct Comment {
    #[sqlx(rename = "postId")]
    post_id: i32,
    body: String,
}

#[derive(Debug, Clone, FromRow)]
struct Line {
    #[sqlx(rename = "orderId")]
    order_id: i32,
    n: i32,
}

#[derive(Debug, Clone, FromRow)]
struct Shipment {
    id: i32,
    #[sqlx(rename = "orderId")]
    order_id: i32,
    n: i32,
}

async fn setup() -> Option<(PgPool, Db, String)> {
    static N: AtomicUsize = AtomicUsize::new(0);
    let url = std::env::var("LUMEN_TEST_POSTGRES_URL").ok()?;
    let admin = PgPool::connect(&url).await.unwrap();
    let name = format!(
        "lumen_rel_{}_{}",
        std::process::id(),
        N.fetch_add(1, AtomicOrdering::Relaxed)
    );
    run_sql(&admin, &format!("drop database if exists {name}")).await;
    run_sql(&admin, &format!("create database {name}")).await;
    let base = url.rsplit_once('/').map_or(url.as_str(), |(b, _)| b);
    let db = Db::connect(&format!("{base}/{name}")).await.unwrap();
    run_sql(db.pool(), SCHEMA).await;
    Some((admin, db, name))
}

async fn run_sql(pool: &PgPool, sql: &str) {
    lumen_orm::sqlx::raw_sql(AssertSqlSafe(sql.to_owned()))
        .execute(pool)
        .await
        .unwrap();
}

async fn finish(admin: PgPool, db: Db, name: String) {
    db.pool().close().await;
    run_sql(
        &admin,
        &format!("drop database if exists {name} with (force)"),
    )
    .await;
}

/// 100 users, 3 posts each, 2 comments per post; users divisible by 3 have a profile.
async fn seed(db: &Db) {
    run_sql(
        db.pool(),
        r#"
        INSERT INTO "User" (name) SELECT 'user ' || g FROM generate_series(1, 100) g;
        INSERT INTO "Profile" ("userId", bio) SELECT id, 'bio ' || id FROM "User" WHERE id % 3 = 0;
        INSERT INTO "Post" ("authorId", title) SELECT u.id, 'post ' || p FROM "User" u, generate_series(1, 3) p ORDER BY u.id, p;
        INSERT INTO "Comment" ("postId", body) SELECT p.id, 'c' || c FROM "Post" p, generate_series(1, 2) c ORDER BY p.id, c;
        "#,
    )
    .await;
}

fn select(table: &str) -> Select {
    let mut s = Select::from(Table::new(table));
    s.order = vec![("id".into(), Order::Asc)];
    s
}

#[tokio::test]
async fn ac_001_a_two_level_include_over_100_parents_runs_3_statements() {
    let Some((admin, db, name)) = setup().await else {
        return;
    };
    seed(&db).await;
    let before = db.statements();
    let mut run = &db;
    let users: Vec<User> = run.fetch_all(select("User").build()).await.unwrap();
    let posts = load_many(
        &mut run,
        &users,
        |u| Some(u.id),
        &select("Post"),
        "authorId",
        |p: &Post| Some(p.author_id),
        None,
    )
    .await
    .unwrap();
    let all_posts: Vec<Post> = posts.iter().flatten().cloned().collect();
    let comments = load_many(
        &mut run,
        &all_posts,
        |p| Some(p.id),
        &select("Comment"),
        "postId",
        |c: &Comment| Some(c.post_id),
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        db.statements() - before,
        3,
        "users, posts, comments: one statement each"
    );
    assert_eq!(users.len(), 100);
    assert!(posts.iter().all(|p| p.len() == 3));
    assert!(comments.iter().all(|c| c.len() == 2));
    assert_eq!(
        posts[41][0].author_id, users[41].id,
        "children line up with their parents"
    );
    assert_eq!(comments[5][1].body, "c2");
    finish(admin, db, name).await;
}

#[tokio::test]
async fn to_one_relations_both_ways() {
    let Some((admin, db, name)) = setup().await else {
        return;
    };
    seed(&db).await;
    let mut run = &db;
    let users: Vec<User> = run.fetch_all(select("User").build()).await.unwrap();
    let profiles = load_one(
        &mut run,
        &users,
        |u| Some(u.id),
        &select("Profile"),
        "userId",
        |p: &Profile| Some(p.user_id),
    )
    .await
    .unwrap();
    assert!(profiles[2].as_ref().is_some_and(|p| p.bio == "bio 3"));
    assert!(profiles[0].is_none());
    let mut first_posts = select("Post");
    first_posts.limit = Some(5);
    let posts: Vec<Post> = run.fetch_all(first_posts.build()).await.unwrap();
    let authors = load_one(
        &mut run,
        &posts,
        |p| Some(p.author_id),
        &select("User"),
        "id",
        |u: &User| Some(u.id),
    )
    .await
    .unwrap();
    assert_eq!(
        authors
            .iter()
            .map(|a| a.as_ref().unwrap().name.as_str())
            .collect::<Vec<_>>(),
        ["user 1", "user 1", "user 1", "user 2", "user 2"]
    );
    finish(admin, db, name).await;
}

#[tokio::test]
async fn take_per_parent_and_child_filters_run_in_sql() {
    let Some((admin, db, name)) = setup().await else {
        return;
    };
    seed(&db).await;
    let mut run = &db;
    let users: Vec<User> = run.fetch_all(select("User").build()).await.unwrap();
    let mut newest = select("Post");
    newest.order = vec![("id".into(), Order::Desc)];
    let before = db.statements();
    let posts = load_many(
        &mut run,
        &users,
        |u| Some(u.id),
        &newest,
        "authorId",
        |p: &Post| Some(p.author_id),
        Some(2),
    )
    .await
    .unwrap();
    assert_eq!(db.statements() - before, 1);
    assert!(posts.iter().all(|p| p.len() == 2));
    assert_eq!(
        posts[0]
            .iter()
            .map(|p| p.title.as_str())
            .collect::<Vec<_>>(),
        ["post 3", "post 2"]
    );

    let mut only_first = select("Post");
    only_first.filter = Some(col("title").equals("post 1"));
    let filtered = load_many(
        &mut run,
        &users,
        |u| Some(u.id),
        &only_first,
        "authorId",
        |p: &Post| Some(p.author_id),
        None,
    )
    .await
    .unwrap();
    assert!(
        filtered
            .iter()
            .all(|p| p.len() == 1 && p[0].title == "post 1")
    );

    let none = load_many::<_, User, Post, i32>(
        &mut run,
        &[],
        |u| Some(u.id),
        &select("Post"),
        "authorId",
        |p| Some(p.author_id),
        None,
    )
    .await
    .unwrap();
    assert!(none.is_empty());
    assert_eq!(db.statements() - before, 2, "no parents, no query");
    finish(admin, db, name).await;
}

#[tokio::test]
async fn composite_keys() {
    let Some((admin, db, name)) = setup().await else {
        return;
    };
    run_sql(db.pool(), r#"INSERT INTO "Line" VALUES (1, 1), (1, 2), (2, 1); INSERT INTO "Shipment" ("orderId", n) VALUES (1, 2), (1, 2), (2, 1);"#).await;
    let mut run = &db;
    let mut lines_q = Select::from(Table::new("Line"));
    lines_q.order = vec![("orderId".into(), Order::Asc), ("n".into(), Order::Asc)];
    let lines: Vec<Line> = run.fetch_all(lines_q.build()).await.unwrap();
    let shipments = load_many_composite(
        &mut run,
        &lines,
        |l| Some((l.order_id, l.n)),
        &select("Shipment"),
        &["orderId".to_owned(), "n".to_owned()],
        |s: &Shipment| (s.order_id, s.n),
        |(o, n)| vec![Value::Int(*o), Value::Int(*n)],
    )
    .await
    .unwrap();
    assert_eq!(
        shipments.iter().map(Vec::len).collect::<Vec<_>>(),
        [0, 2, 1]
    );
    assert!(shipments[1].iter().all(|s| s.id > 0));
    finish(admin, db, name).await;
}

fn insert_user(name: &str) -> lumen_orm::Built {
    Insert {
        table: Table::new("User"),
        columns: vec!["name".into()],
        rows: vec![vec![name.into()]],
        on_conflict: None,
        returning: vec!["id".into(), "name".into()],
    }
    .build()
}

async fn users_named(db: &Db, name: &str) -> i64 {
    let mut q = Select::from(Table::new("User"));
    q.filter = Some(col("name").equals(name));
    db.count(&q).await.unwrap()
}

#[tokio::test]
async fn ac_003_a_failed_nested_create_leaves_no_rows() {
    let Some((admin, db, name)) = setup().await else {
        return;
    };
    let result = db
        .transaction(async |tx| {
            let user: User = tx.fetch_one(insert_user("Ana")).await?;
            // the post points at a user that does not exist: P2003, so nothing is kept
            let post = Insert {
                table: Table::new("Post"),
                columns: vec!["authorId".into(), "title".into()],
                rows: vec![vec![(user.id + 1000).into(), "x".into()]],
                on_conflict: None,
                returning: vec![],
            };
            tx.execute(post.build()).await?;
            Ok(user)
        })
        .await;
    assert_eq!(result.unwrap_err().code, "P2003");
    assert_eq!(
        users_named(&db, "Ana").await,
        0,
        "the user was rolled back with the post"
    );

    let kept = db
        .transaction(async |tx| {
            let user: User = tx.fetch_one(insert_user("Bo")).await?;
            let post = Insert {
                table: Table::new("Post"),
                columns: vec!["authorId".into(), "title".into()],
                rows: vec![vec![user.id.into(), "hi".into()]],
                on_conflict: None,
                returning: vec![],
            };
            tx.execute(post.build()).await?;
            Ok(user.id)
        })
        .await
        .unwrap();
    assert!(kept > 0);
    assert_eq!(users_named(&db, "Bo").await, 1);
    finish(admin, db, name).await;
}

#[tokio::test]
async fn savepoints_and_isolation() {
    let Some((admin, db, name)) = setup().await else {
        return;
    };
    db.transaction(async |tx| {
        tx.fetch_one::<User>(insert_user("outer")).await?;
        let inner: Result<(), Error> = tx
            .transaction(async |inner| {
                inner.fetch_one::<User>(insert_user("inner")).await?;
                Err(Error::new("P2010", "changed my mind"))
            })
            .await;
        assert!(inner.is_err());
        Ok(())
    })
    .await
    .unwrap();
    assert_eq!(
        users_named(&db, "outer").await,
        1,
        "the outer transaction keeps its work"
    );
    assert_eq!(
        users_named(&db, "inner").await,
        0,
        "the savepoint was rolled back"
    );

    let n = db
        .transaction_with(Isolation::Serializable, async |tx| {
            let level: Vec<Level> = tx
                .fetch_all(lumen_orm::Built {
                    sql: "SHOW transaction_isolation".into(),
                    params: vec![],
                })
                .await?;
            Ok(level[0].transaction_isolation.clone())
        })
        .await
        .unwrap();
    assert_eq!(n, "serializable");
    finish(admin, db, name).await;
}

#[derive(FromRow)]
struct Level {
    transaction_isolation: String,
}
