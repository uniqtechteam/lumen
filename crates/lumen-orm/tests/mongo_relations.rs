//! SPEC-MONGO-001 FR-003, FR-004 on a real MongoDB replica set: relations in one `$in` query per
//! level, `take` per parent on the server, and transactions. Runs when `LUMEN_TEST_MONGO_URL` is set.

#![allow(clippy::unwrap_used, reason = "tests: a failure should stop the test")]

use std::sync::atomic::{AtomicUsize, Ordering};

use lumen_orm::mongo::{Find, MongoDb, MongoRun, load_many, load_one};
use lumen_orm::{Error, Order, col};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct User {
    id: i32,
    name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Post {
    id: i32,
    #[serde(rename = "authorId")]
    author_id: i32,
    title: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Comment {
    #[serde(rename = "postId")]
    post_id: i32,
    body: String,
}

async fn setup() -> Option<MongoDb> {
    static N: AtomicUsize = AtomicUsize::new(0);
    let url = std::env::var("LUMEN_TEST_MONGO_URL").ok()?;
    let name = format!(
        "lumen_mrel_{}_{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    );
    let db = MongoDb::connect(&url, &name).await.unwrap();
    db.database().drop().await.unwrap();
    let users: Vec<User> = (1..=100)
        .map(|i| User {
            id: i,
            name: format!("user {i}"),
        })
        .collect();
    let posts: Vec<Post> = users
        .iter()
        .flat_map(|u| {
            (1..=3).map(move |p| Post {
                id: u.id * 10 + p,
                author_id: u.id,
                title: format!("post {p}"),
            })
        })
        .collect();
    let comments: Vec<Comment> = posts
        .iter()
        .flat_map(|p| {
            (1..=2).map(move |c| Comment {
                post_id: p.id,
                body: format!("c{c}"),
            })
        })
        .collect();
    db.insert_many("User", &users).await.unwrap();
    db.insert_many("Post", &posts).await.unwrap();
    db.insert_many("Comment", &comments).await.unwrap();
    Some(db)
}

fn by_id(collection: &str) -> Find {
    Find {
        collection: collection.into(),
        order: vec![("id".into(), Order::Asc)],
        ..Find::default()
    }
}

#[tokio::test]
async fn a_two_level_include_over_100_parents_runs_3_operations() {
    let Some(db) = setup().await else {
        eprintln!("skipped: LUMEN_TEST_MONGO_URL is not set");
        return;
    };
    let before = db.statements();
    let mut run = &db;
    let users: Vec<User> = run.find(&by_id("User")).await.unwrap();
    let posts = load_many(
        &mut run,
        &users,
        |u| Some(u.id),
        &by_id("Post"),
        "authorId",
        |p: &Post| Some(p.author_id),
        None,
    )
    .await
    .unwrap();
    let all: Vec<Post> = posts.iter().flatten().cloned().collect();
    let comments = load_many(
        &mut run,
        &all,
        |p| Some(p.id),
        &Find {
            collection: "Comment".into(),
            ..Find::default()
        },
        "postId",
        |c: &Comment| Some(c.post_id),
        None,
    )
    .await
    .unwrap();
    assert_eq!(db.statements() - before, 3);
    assert!(posts.iter().all(|p| p.len() == 3));
    assert!(comments.iter().all(|c| c.len() == 2));
    assert_eq!(posts[41][0].author_id, users[41].id);

    let mut newest = by_id("Post");
    newest.order = vec![("id".into(), Order::Desc)];
    let two = load_many(
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
    assert!(
        two.iter().all(|p| p.len() == 2),
        "take per parent on the server"
    );
    assert_eq!(
        two[0].iter().map(|p| p.title.as_str()).collect::<Vec<_>>(),
        ["post 3", "post 2"]
    );

    let authors = load_one(
        &mut run,
        &all[..4],
        |p| Some(p.author_id),
        &by_id("User"),
        "id",
        |u: &User| Some(u.id),
    )
    .await
    .unwrap();
    assert_eq!(
        authors
            .iter()
            .map(|a| a.as_ref().unwrap().id)
            .collect::<Vec<_>>(),
        [1, 1, 1, 2]
    );
    db.database().drop().await.unwrap();
}

#[tokio::test]
async fn ac_fr_004_skip_and_take_inside_an_include_page_each_parent_on_the_server() {
    let Some(db) = setup().await else { return };
    let mut run = &db;
    let users: Vec<User> = run.find(&by_id("User")).await.unwrap();
    let mut page = by_id("Post");
    page.order = vec![("id".into(), Order::Desc)];
    page.skip = Some(1);
    page.limit = Some(1);
    let before = db.statements();
    let posts = load_many(
        &mut run,
        &users,
        |u| Some(u.id),
        &page,
        "authorId",
        |p: &Post| Some(p.author_id),
        None,
    )
    .await
    .unwrap();
    assert_eq!(db.statements() - before, 1, "one operation for all parents");
    assert!(
        posts.iter().all(|p| p.len() == 1 && p[0].title == "post 2"),
        "the second-newest post of every user, not one post overall"
    );
    page.limit = None;
    page.skip = Some(2);
    let last = load_many(
        &mut run,
        &users,
        |u| Some(u.id),
        &page,
        "authorId",
        |p: &Post| Some(p.author_id),
        None,
    )
    .await
    .unwrap();
    assert!(last.iter().all(|p| p.len() == 1 && p[0].title == "post 1"));
    page.skip = Some(1);
    let two = load_many(
        &mut run,
        &users,
        |u| Some(u.id),
        &page,
        "authorId",
        |p: &Post| Some(p.author_id),
        Some(5),
    )
    .await
    .unwrap();
    assert!(two.iter().all(|p| p.len() == 2), "3 posts, skip 1");
    db.database().drop().await.unwrap();
}

#[tokio::test]
async fn ac_fr_004_paging_an_include_that_cannot_run_is_an_error() {
    let Some(db) = setup().await else { return };
    let mut run = &db;
    let posts: Vec<Post> = run.find(&by_id("Post")).await.unwrap();
    let mut paged = by_id("User");
    paged.skip = Some(1);
    let before = db.statements();
    let err = load_one(
        &mut run,
        &posts,
        |p| Some(p.author_id),
        &paged,
        "id",
        |u: &User| Some(u.id),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "P2009");
    let users: Vec<User> = run.find(&by_id("User")).await.unwrap();
    let err = load_many(
        &mut run,
        &users,
        |u| Some(u.id),
        &by_id("Post"),
        "authorId",
        |p: &Post| Some(p.author_id),
        Some(-1),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "P2009");
    assert_eq!(db.statements() - before, 1, "only the users were read");
    db.database().drop().await.unwrap();
}

#[tokio::test]
async fn transactions_commit_or_leave_nothing() {
    let Some(db) = setup().await else { return };
    let failed: lumen_orm::Result<()> = db
        .transaction(async |tx| {
            tx.insert_many(
                "User",
                &[User {
                    id: 500,
                    name: "Ana".into(),
                }],
            )
            .await?;
            tx.update_many(
                "Post",
                Some(&col("authorId").equals(1)),
                vec![("title".into(), "changed".into())],
            )
            .await?;
            Err(Error::new("P2010", "changed my mind"))
        })
        .await;
    assert!(failed.is_err());
    assert_eq!(
        db.count("User", Some(&col("id").equals(500)))
            .await
            .unwrap(),
        0,
        "the insert was rolled back"
    );
    assert_eq!(
        db.count("Post", Some(&col("title").equals("changed")))
            .await
            .unwrap(),
        0,
        "and the update"
    );

    let n = db
        .transaction(async |tx| {
            tx.insert_many(
                "User",
                &[User {
                    id: 501,
                    name: "Bo".into(),
                }],
            )
            .await?;
            tx.count("User", None).await
        })
        .await
        .unwrap();
    assert_eq!(n, 101, "the transaction sees its own insert");
    assert_eq!(
        db.count("User", Some(&col("id").equals(501)))
            .await
            .unwrap(),
        1
    );
    db.database().drop().await.unwrap();
}
