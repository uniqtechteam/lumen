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
        lumen_codegen::generate(&schema, "shop.prisma").unwrap(),
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
    let once = lumen_codegen::generate(&schema, "blog.prisma").unwrap();
    let twice = lumen_codegen::generate(&schema, "blog.prisma").unwrap();
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
    use super::db::{self, Client, Role, post, profile, user};
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
}
