//! SPEC-GEN-001: the client generated for `tests/blog.prisma` compiles, is up to date, and works
//! against a real Postgres (when `LUMEN_TEST_POSTGRES_URL` is set).

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::too_many_lines,
    reason = "tests: a failure should stop the test; each live test is one scenario"
)]

#[rustfmt::skip]
#[path = "generated/blog.rs"]
mod db;

#[rustfmt::skip]
#[path = "generated/shop.rs"]
#[allow(dead_code)]
mod shop_db;

#[test]
fn ac_001_every_committed_client_is_current() {
    let text = include_str!("shop.prisma");
    let (schema, d) = lumen_schema::check(&lumen_schema::SourceFile::new("shop.prisma", text));
    assert!(!d.has_errors());
    assert_eq!(
        lumen_db_codegen::generate(&schema, "shop.prisma").unwrap(),
        include_str!("generated/shop.rs"),
        "tests/generated/shop.rs is out of date: regenerate it with the gen example"
    );
    // a compound primary key becomes a struct variant; schemas and native types carry over
    let _ = shop_db::order_line::UniqueWhere::OrderIdProductId {
        order_id: 1,
        product_id: lumen_orm::uuid::Uuid::nil(),
    };
    assert_eq!(shop_db::product::table().schema.as_deref(), Some("public"));
}

#[test]
fn ac_003_the_generated_file_is_current_and_stable() {
    let text = include_str!("blog.prisma");
    let (schema, d) = lumen_schema::check(&lumen_schema::SourceFile::new("blog.prisma", text));
    assert!(!d.has_errors());
    let once = lumen_db_codegen::generate(&schema, "blog.prisma").unwrap();
    let twice = lumen_db_codegen::generate(&schema, "blog.prisma").unwrap();
    assert_eq!(once, twice, "generating twice gives the same bytes");
    assert_eq!(
        once,
        include_str!("generated/blog.rs"),
        "tests/generated/blog.rs is out of date: regenerate it"
    );
}

#[test]
fn ac_002_a_wrong_value_type_does_not_compile() {
    trybuild::TestCases::new().compile_fail("tests/ui/*.rs");
}

mod live {
    use super::db::{self, Client, Role, post, profile, tag, user};
    use lumen_orm::sqlx::{AssertSqlSafe, PgPool};
    use lumen_orm::{Run, Set};

    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    async fn client() -> Option<(PgPool, Client, String)> {
        let url = std::env::var("LUMEN_TEST_POSTGRES_URL").ok()?;
        let admin = PgPool::connect(&url).await.unwrap();
        let name = format!(
            "lumen_gen_{}_{}",
            std::process::id(),
            N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
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
        let db_url = format!("{base}/{name}");
        let (schema, _) = lumen_schema::check(&lumen_schema::SourceFile::new(
            "blog.prisma",
            include_str!("blog.prisma"),
        ));
        let steps = lumen_migrate::diff(
            &lumen_migrate::Shape::default(),
            &lumen_migrate::calculate(&schema),
            &lumen_migrate::Renames::default(),
        );
        let pool = PgPool::connect(&db_url).await.unwrap();
        lumen_orm::sqlx::raw_sql(AssertSqlSafe(lumen_migrate::postgres::script(&steps, &[])))
            .execute(&pool)
            .await
            .unwrap();
        Some((admin, Client::new(lumen_orm::Db::from_pool(pool)), name))
    }

    async fn finish(admin: PgPool, client: Client, name: String) {
        client.db().pool().close().await;
        lumen_orm::sqlx::raw_sql(AssertSqlSafe(format!(
            "drop database if exists {name} with (force)"
        )))
        .execute(&admin)
        .await
        .unwrap();
    }

    fn new_user(email: &str) -> user::Create {
        user::Create {
            id: None,
            email: email.into(),
            name: None,
            role: None,
            created_at: None,
            updated_at: None,
        }
    }

    #[tokio::test]
    async fn crud_filters_and_coded_errors() {
        let Some((admin, client, name)) = client().await else {
            return;
        };
        let ana = client
            .user()
            .create(user::Create {
                name: Some("Ana".into()),
                ..new_user("ana@example.com")
            })
            .await
            .unwrap();
        assert_eq!(ana.role, Role::Reader, "the database default");
        assert!(ana.id > 0);
        client
            .user()
            .create(user::Create {
                role: Some(Role::Writer),
                ..new_user("bo@example.com")
            })
            .await
            .unwrap();
        client
            .user()
            .create_many(vec![new_user("cy@test.dev"), new_user("di@test.dev")])
            .await
            .unwrap();

        let found = client
            .user()
            .find_unique(user::UniqueWhere::Email("bo@example.com".into()))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(found.role, Role::Writer);
        let writers_first = client
            .user()
            .find_many()
            .filter(user::email::ends_with("@example.com"))
            .order_by(user::role::desc())
            .exec()
            .await
            .unwrap();
        assert_eq!(
            writers_first
                .iter()
                .map(|u| u.email.as_str())
                .collect::<Vec<_>>(),
            ["bo@example.com", "ana@example.com"]
        );
        assert_eq!(
            client
                .user()
                .count(Some(user::name::is_null()))
                .await
                .unwrap(),
            3
        );

        let changed = client
            .user()
            .update(
                user::UniqueWhere::Id(ana.id),
                user::Update {
                    name: Set::To(Some("Ana Lima".into())),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(changed.name.as_deref(), Some("Ana Lima"));
        assert!(
            changed.updated_at >= ana.updated_at,
            "@updatedAt is set on every update"
        );

        let upserted = client
            .user()
            .upsert(
                user::UniqueWhere::Email("ana@example.com".into()),
                new_user("ana@example.com"),
                user::Update {
                    role: Set::To(Role::Admin),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(
            (upserted.id, upserted.role),
            (ana.id, Role::Admin),
            "the existing row is updated"
        );

        let e = client
            .user()
            .create(new_user("bo@example.com"))
            .await
            .unwrap_err();
        assert_eq!(
            (e.code, e.fields.clone()),
            ("P2002", vec!["email".to_owned()])
        );
        let missing = client
            .user()
            .delete(user::UniqueWhere::Id(999))
            .await
            .unwrap_err();
        assert_eq!(missing.code, "P2025");
        assert_eq!(
            client
                .user()
                .delete_many(Some(user::email::ends_with("@test.dev")))
                .await
                .unwrap(),
            2
        );
        finish(admin, client, name).await;
    }

    #[tokio::test]
    async fn includes_and_transactions() {
        let Some((admin, client, name)) = client().await else {
            return;
        };
        let ana = client
            .user()
            .create(new_user("ana@example.com"))
            .await
            .unwrap();
        let bo = client
            .user()
            .create(new_user("bo@example.com"))
            .await
            .unwrap();
        for (author, title) in [(ana.id, "one"), (ana.id, "two"), (bo.id, "three")] {
            client
                .post()
                .create(post::Create {
                    id: None,
                    title: title.into(),
                    body: None,
                    published: None,
                    author_id: author,
                    views: None,
                })
                .await
                .unwrap();
        }
        client
            .profile()
            .create(profile::Create {
                id: None,
                bio: "hi".into(),
                user_id: ana.id,
            })
            .await
            .unwrap();

        let before = client.db().statements();
        let users = client
            .user()
            .find_many()
            .order_by(user::id::asc())
            .include_posts()
            .order_by(post::id::desc())
            .exec()
            .await
            .unwrap();
        assert_eq!(
            client.db().statements() - before,
            2,
            "users, then all their posts"
        );
        assert_eq!(
            users[0]
                .posts
                .iter()
                .map(|p| p.title.as_str())
                .collect::<Vec<_>>(),
            ["two", "one"]
        );
        assert_eq!(users[1].posts.len(), 1);
        let json = lumen_orm::serde_json::to_value(&users[1]).unwrap();
        assert_eq!(json["email"], "bo@example.com", "the record is flattened");
        assert_eq!(json["posts"][0]["title"], "three");

        // SPEC-QUERY-001 FR-004: skip and take inside an include page each user's posts
        let paged = client
            .user()
            .find_many()
            .order_by(user::id::asc())
            .include_posts()
            .order_by(post::id::desc())
            .skip(1)
            .take(1)
            .exec()
            .await
            .unwrap();
        assert_eq!(
            paged
                .iter()
                .map(|u| u.posts.iter().map(|p| p.title.as_str()).collect::<Vec<_>>())
                .collect::<Vec<_>>(),
            [vec!["one"], vec![]]
        );

        let posts = client
            .post()
            .find_many()
            .include_author()
            .exec()
            .await
            .unwrap();
        assert!(
            posts.iter().all(|p| p.author.id == p.post.author_id),
            "a required to-one is not an Option"
        );
        let with_profile = client
            .user()
            .find_many()
            .order_by(user::id::asc())
            .include_profile()
            .exec()
            .await
            .unwrap();
        assert_eq!(
            with_profile[0].profile.as_ref().map(|p| p.bio.as_str()),
            Some("hi")
        );
        assert!(with_profile[1].profile.is_none());

        let result: lumen_orm::Result<()> = client
            .db()
            .transaction(async |tx| {
                let cy = user::on(&mut *tx)
                    .create(new_user("cy@example.com"))
                    .await?;
                post::on(&mut *tx)
                    .create(post::Create {
                        id: None,
                        title: "x".into(),
                        body: None,
                        published: None,
                        author_id: cy.id + 100,
                        views: None,
                    })
                    .await?;
                Ok(())
            })
            .await;
        assert_eq!(result.unwrap_err().code, "P2003");
        assert!(
            client
                .user()
                .find_unique(user::UniqueWhere::Email("cy@example.com".into()))
                .await
                .unwrap()
                .is_none(),
            "rolled back"
        );
        let _ = db::Client::new(client.db().clone());
        let mut db_ref = client.db();
        assert_eq!(Run::count(&mut db_ref, &user::select()).await.unwrap(), 2);
        finish(admin, client, name).await;
    }

    fn emails(users: &[user::User]) -> Vec<&str> {
        users.iter().map(|u| u.email.as_str()).collect()
    }

    /// SPEC-QUERY-001 FR-002: `some`, `every`, `none`, `is`, `is_not`, `is_null` through the
    /// generated client, on one-to-many, one-to-one and many-to-many relations, nested.
    #[tokio::test]
    async fn ac_fr_002_relation_filters_through_the_generated_client() {
        let Some((admin, client, name)) = client().await else {
            return;
        };
        let mut ids = Vec::new();
        for email in ["ana@example.com", "bo@example.com", "cy@example.com"] {
            ids.push(client.user().create(new_user(email)).await.unwrap().id);
        }
        let mut posts = Vec::new();
        for (author, title, published) in [(0, "one", true), (0, "two", false), (1, "three", true)]
        {
            let p = post::Create {
                id: None,
                title: title.into(),
                body: None,
                published: Some(published),
                author_id: ids[author],
                views: None,
            };
            posts.push(client.post().create(p).await.unwrap().id);
        }
        client
            .profile()
            .create(profile::Create {
                id: None,
                bio: "hi there".into(),
                user_id: ids[0],
            })
            .await
            .unwrap();
        let tags = format!(
            "INSERT INTO \"Tag\" VALUES ('rust'), ('go'); INSERT INTO \"_PostToTag\" VALUES ({}, 'rust'), ({}, 'go'), ({}, 'rust')",
            posts[0], posts[1], posts[2]
        );
        lumen_orm::sqlx::raw_sql(AssertSqlSafe(tags))
            .execute(client.db().pool())
            .await
            .unwrap();

        let users = async |f: lumen_orm::Filter| {
            let found = client
                .user()
                .find_many()
                .filter(f)
                .order_by(user::id::asc());
            found.exec().await.unwrap()
        };
        let published = || post::published::equals(true);
        assert_eq!(
            emails(&users(user::posts::some(published())).await),
            ["ana@example.com", "bo@example.com"]
        );
        assert_eq!(
            emails(&users(user::posts::every(published())).await),
            ["bo@example.com", "cy@example.com"],
            "every is true for a user without posts"
        );
        assert_eq!(
            emails(&users(user::posts::none(published())).await),
            ["cy@example.com"]
        );
        assert_eq!(
            emails(
                &users(user::profile::is(profile::bio::starts_with_insensitive(
                    "HI"
                )))
                .await
            ),
            ["ana@example.com"]
        );
        assert_eq!(
            emails(&users(user::profile::is_null()).await),
            ["bo@example.com", "cy@example.com"]
        );
        assert_eq!(
            emails(&users(user::posts::some(post::tags::some(tag::name::equals("go")))).await),
            ["ana@example.com"],
            "nested, through the many-to-many join table"
        );
        assert_eq!(
            emails(&users(!user::profile::is_not_null().or(user::posts::none(published()))).await),
            ["bo@example.com"],
            "inside NOT and OR"
        );

        let titles = async |f: lumen_orm::Filter| {
            let found = client
                .post()
                .find_many()
                .filter(f)
                .order_by(post::id::asc());
            let found = found.exec().await.unwrap();
            found.into_iter().map(|p| p.title).collect::<Vec<_>>()
        };
        assert_eq!(
            titles(post::author::is(user::email::ends_with_insensitive(
                "@EXAMPLE.COM"
            )))
            .await,
            ["one", "two", "three"]
        );
        assert_eq!(
            titles(post::author::is_not(user::profile::is_not_null())).await,
            ["three"]
        );
        assert_eq!(
            titles(post::tags::every(tag::name::equals("rust"))).await,
            ["one", "three"]
        );
        assert_eq!(
            client
                .post()
                .count(Some(post::tags::none(lumen_orm::Filter::And(vec![]))))
                .await
                .unwrap(),
            0
        );
        assert_eq!(
            client
                .tag()
                .delete_many(Some(tag::posts::every(post::published::equals(false))))
                .await
                .unwrap(),
            1,
            "only `go` is on unpublished posts alone"
        );
        finish(admin, client, name).await;
    }
}
