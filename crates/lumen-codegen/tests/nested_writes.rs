//! SPEC-QUERY-001 FR-005 and AC-003 through the generated clients: `create_nested` and
//! `create_with_<relation>` create or connect related records in one transaction, in the order the
//! foreign key needs, and a failure keeps nothing. Runs on Postgres (`LUMEN_TEST_POSTGRES_URL`)
//! and on a MongoDB replica set (`LUMEN_TEST_MONGO_URL`).

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::too_many_lines,
    reason = "tests: a failure should stop the test; each live test is one scenario"
)]

#[rustfmt::skip]
#[path = "generated/blog.rs"]
#[allow(dead_code)]
mod blog;

#[rustfmt::skip]
#[path = "generated/mongo.rs"]
#[allow(dead_code)]
mod mongo;

use lumen_orm::nested::{Many, One};

mod postgres {
    use super::blog::{Client, post, profile, user};
    use super::{Many, One};
    use lumen_orm::sqlx::{AssertSqlSafe, PgPool};

    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    async fn client() -> Option<(PgPool, Client, String)> {
        let url = std::env::var("LUMEN_TEST_POSTGRES_URL").ok()?;
        let admin = PgPool::connect(&url).await.unwrap();
        let name = format!(
            "lumen_nested_{}_{}",
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
        let (schema, _) = lumen_schema::check(&lumen_schema::SourceFile::new(
            "blog.prisma",
            include_str!("blog.prisma"),
        ));
        let steps = lumen_migrate::diff(
            &lumen_migrate::Shape::default(),
            &lumen_migrate::calculate(&schema),
            &lumen_migrate::Renames::default(),
        );
        let pool = PgPool::connect(&format!("{base}/{name}")).await.unwrap();
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

    fn new_post(title: &str) -> post::CreateWithoutAuthor {
        post::CreateWithoutAuthor {
            id: None,
            title: title.into(),
            body: None,
            published: None,
            views: None,
        }
    }

    async fn counts(client: &Client) -> (i64, i64, i64) {
        (
            client.user().count(None).await.unwrap(),
            client.post().count(None).await.unwrap(),
            client.profile().count(None).await.unwrap(),
        )
    }

    #[tokio::test]
    async fn ac_fr_005_nested_creates_and_connects_follow_the_foreign_key() {
        let Some((admin, client, name)) = client().await else {
            return;
        };
        // the parent first, then its children with the key: one statement for all the posts
        let before = client.db().statements();
        let ana = client
            .user()
            .create_nested(
                new_user("ana@example.com"),
                user::Nested {
                    posts: Many::create(vec![new_post("one"), new_post("two")]),
                    profile: Some(One::Create(profile::CreateWithoutUser {
                        id: None,
                        bio: "writes".into(),
                    })),
                },
            )
            .await
            .unwrap();
        assert_eq!(client.db().statements() - before, 3);
        let found = client
            .user()
            .find_many()
            .filter(user::id::equals(ana.id))
            .include_posts()
            .order_by(post::title::asc())
            .exec()
            .await
            .unwrap();
        let titles: Vec<&str> = found[0].posts.iter().map(|p| p.title.as_str()).collect();
        assert_eq!(titles, ["one", "two"]);

        // the side holding the key: the author is found (or created) first
        let three = client
            .post()
            .create_with_author(
                new_post("three"),
                One::Connect(user::UniqueWhere::Email("ana@example.com".into())),
            )
            .await
            .unwrap();
        assert_eq!(three.author_id, ana.id);
        let four = client
            .post()
            .create_with_author(new_post("four"), One::Create(new_user("bo@example.com")))
            .await
            .unwrap();
        let bo = client
            .user()
            .find_unique(user::UniqueWhere::Email("bo@example.com".into()))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(four.author_id, bo.id);

        // connect moves existing records: two posts and ana's profile go to cy
        let ana_profile = client
            .profile()
            .find_unique(profile::UniqueWhere::UserId(ana.id))
            .await
            .unwrap()
            .unwrap();
        let cy = client
            .user()
            .create_nested(
                new_user("cy@example.com"),
                user::Nested {
                    posts: Many::create(vec![new_post("five")]).and_connect(vec![
                        post::UniqueWhere::Id(three.id),
                        post::UniqueWhere::OneTitlePerAuthor {
                            author_id: bo.id,
                            title: "four".into(),
                        },
                    ]),
                    profile: Some(One::Connect(profile::UniqueWhere::Id(ana_profile.id))),
                },
            )
            .await
            .unwrap();
        let cy_posts = client
            .post()
            .find_many()
            .filter(post::author_id::equals(cy.id))
            .order_by(post::title::asc())
            .exec()
            .await
            .unwrap();
        let titles: Vec<&str> = cy_posts.iter().map(|p| p.title.as_str()).collect();
        assert_eq!(titles, ["five", "four", "three"]);
        let moved = client
            .profile()
            .find_unique(profile::UniqueWhere::Id(ana_profile.id))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(moved.user_id, cy.id);

        // a one-to-one from the side holding the key
        let dee = client
            .profile()
            .create_with_user(
                profile::CreateWithoutUser {
                    id: None,
                    bio: "new".into(),
                },
                One::Create(new_user("dee@example.com")),
            )
            .await
            .unwrap();
        let dee_user = client
            .user()
            .find_unique(user::UniqueWhere::Id(dee.user_id))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(dee_user.email, "dee@example.com");

        // inside a transaction the nested write is a savepoint of it
        let id = client
            .db()
            .transaction(async |tx| {
                let eve = user::on(&mut *tx)
                    .create_nested(
                        new_user("eve@example.com"),
                        user::Nested {
                            posts: Many::create(vec![new_post("six")]),
                            ..user::Nested::default()
                        },
                    )
                    .await?;
                Ok(eve.id)
            })
            .await
            .unwrap();
        assert_eq!(
            client
                .post()
                .count(Some(post::author_id::equals(id)))
                .await
                .unwrap(),
            1
        );
        finish(admin, client, name).await;
    }

    #[tokio::test]
    async fn ac_003_a_failed_nested_create_leaves_no_rows() {
        let Some((admin, client, name)) = client().await else {
            return;
        };
        let ana = client
            .user()
            .create(new_user("ana@example.com"))
            .await
            .unwrap();
        let first = client
            .post()
            .create(new_post("first").into_create(ana.id))
            .await
            .unwrap();
        let before = counts(&client).await;

        // the second post breaks the unique title per author: the user and the first post go too
        let err = client
            .user()
            .create_nested(
                new_user("bo@example.com"),
                user::Nested {
                    posts: Many::create(vec![new_post("same"), new_post("same")]),
                    profile: Some(One::Create(profile::CreateWithoutUser {
                        id: None,
                        bio: "x".into(),
                    })),
                },
            )
            .await
            .unwrap_err();
        assert_eq!(err.code, "P2002");
        assert_eq!(err.fields, ["authorId", "title"]);
        assert_eq!(counts(&client).await, before, "nothing was kept");

        // a record to connect that does not exist: P2025, and the new user is rolled back
        let err = client
            .user()
            .create_nested(
                new_user("cy@example.com"),
                user::Nested {
                    posts: Many::create(vec![new_post("kept?")])
                        .and_connect(vec![post::UniqueWhere::Id(999)]),
                    profile: None,
                },
            )
            .await
            .unwrap_err();
        assert_eq!(err.code, "P2025");
        assert!(err.message.contains("Post record to connect"), "{err}");
        assert_eq!(counts(&client).await, before);

        // the side holding the key: a missing author, or a post that fails after its author was made
        let err = client
            .post()
            .create_with_author(
                new_post("x"),
                One::Connect(user::UniqueWhere::Email("nobody@example.com".into())),
            )
            .await
            .unwrap_err();
        assert_eq!(err.code, "P2025");
        let err = client
            .post()
            .create_with_author(
                post::CreateWithoutAuthor {
                    id: Some(first.id),
                    ..new_post("y")
                },
                One::Create(new_user("zed@example.com")),
            )
            .await
            .unwrap_err();
        assert_eq!(err.code, "P2002", "the post's id is taken");
        assert_eq!(
            counts(&client).await,
            before,
            "the new author is rolled back"
        );

        // inside a transaction: the failed nested write is undone, what came before is kept
        client
            .db()
            .transaction(async |tx| {
                post::on(&mut *tx)
                    .create(new_post("before").into_create(ana.id))
                    .await?;
                let failed = user::on(&mut *tx)
                    .create_nested(
                        new_user("dee@example.com"),
                        user::Nested {
                            posts: Many::connect(vec![post::UniqueWhere::Id(999)]),
                            profile: None,
                        },
                    )
                    .await;
                assert_eq!(failed.unwrap_err().code, "P2025");
                Ok(())
            })
            .await
            .unwrap();
        assert_eq!(counts(&client).await, (before.0, before.1 + 1, before.2));
        finish(admin, client, name).await;
    }
}

mod mongodb {
    use super::mongo::{Client, account, session, settings};
    use super::{Many, One};

    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    async fn client() -> Option<Client> {
        let url = std::env::var("LUMEN_TEST_MONGO_URL").ok()?;
        let name = format!(
            "lumen_nestedm_{}_{}",
            std::process::id(),
            N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        let client = Client::connect(&url, &name).await.unwrap();
        client.db().database().drop().await.unwrap();
        let (schema, _) = lumen_schema::check(&lumen_schema::SourceFile::new(
            "mongo.prisma",
            include_str!("mongo.prisma"),
        ));
        lumen_migrate::mongo::push(client.db().database(), &schema)
            .await
            .unwrap();
        Some(client)
    }

    fn new_account(email: &str) -> account::Create {
        account::Create {
            id: None,
            email: email.into(),
            address: None,
            phones: None,
            created_at: None,
        }
    }

    fn new_session(token: &str) -> session::CreateWithoutAccount {
        session::CreateWithoutAccount {
            id: None,
            token: token.into(),
        }
    }

    async fn counts(client: &Client) -> (u64, u64, u64) {
        (
            client.account().count(None).await.unwrap(),
            client.session().count(None).await.unwrap(),
            client.settings().count(None).await.unwrap(),
        )
    }

    #[tokio::test]
    async fn ac_fr_005_nested_creates_and_connects_follow_the_foreign_key() {
        let Some(client) = client().await else { return };
        let ana = client
            .account()
            .create_nested(
                new_account("ana@example.com"),
                account::Nested {
                    sessions: Many::create(vec![new_session("t1"), new_session("t2")]),
                    settings: Some(One::Create(settings::CreateWithoutAccount {
                        id: None,
                        theme: Some("dark".into()),
                    })),
                },
            )
            .await
            .unwrap();
        let found = client
            .account()
            .find_many()
            .include_sessions()
            .order_by(session::token::asc())
            .exec()
            .await
            .unwrap();
        let tokens: Vec<&str> = found[0].sessions.iter().map(|s| s.token.as_str()).collect();
        assert_eq!(tokens, ["t1", "t2"]);
        let ana_settings = client
            .settings()
            .find_unique(settings::UniqueWhere::AccountId(ana.id))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(ana_settings.theme, "dark");

        let t3 = client
            .session()
            .create_with_account(
                new_session("t3"),
                One::Connect(account::UniqueWhere::Email("ana@example.com".into())),
            )
            .await
            .unwrap();
        assert_eq!(t3.account_id, ana.id);
        let t4 = client
            .session()
            .create_with_account(
                new_session("t4"),
                One::Create(new_account("bo@example.com")),
            )
            .await
            .unwrap();
        assert_ne!(t4.account_id, ana.id);

        let cy = client
            .account()
            .create_nested(
                new_account("cy@example.com"),
                account::Nested {
                    sessions: Many::connect(vec![
                        session::UniqueWhere::Token("t3".into()),
                        session::UniqueWhere::Id(t4.id),
                    ]),
                    settings: Some(One::Connect(settings::UniqueWhere::Id(ana_settings.id))),
                },
            )
            .await
            .unwrap();
        assert_eq!(
            client
                .session()
                .count(Some(session::account_id::equals(cy.id)))
                .await
                .unwrap(),
            2
        );
        let moved = client
            .settings()
            .find_unique(settings::UniqueWhere::Id(ana_settings.id))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(moved.account_id, cy.id);
        client.db().database().drop().await.unwrap();
    }

    #[tokio::test]
    async fn ac_003_a_failed_nested_create_leaves_no_documents() {
        let Some(client) = client().await else { return };
        client
            .account()
            .create(new_account("ana@example.com"))
            .await
            .unwrap();
        let before = counts(&client).await;
        let err = client
            .account()
            .create_nested(
                new_account("bo@example.com"),
                account::Nested {
                    sessions: Many::create(vec![new_session("same"), new_session("same")]),
                    settings: None,
                },
            )
            .await
            .unwrap_err();
        assert_eq!(err.code, "P2002");
        assert_eq!(counts(&client).await, before, "nothing was kept");

        let err = client
            .account()
            .create_nested(
                new_account("cy@example.com"),
                account::Nested {
                    sessions: Many::create(vec![new_session("kept?")]),
                    settings: Some(One::Connect(settings::UniqueWhere::Id(
                        lumen_orm::mongo::ObjectId::new(),
                    ))),
                },
            )
            .await
            .unwrap_err();
        assert_eq!(err.code, "P2025");
        assert_eq!(counts(&client).await, before);

        let err = client
            .session()
            .create_with_account(
                new_session("x"),
                One::Connect(account::UniqueWhere::Email("nobody@example.com".into())),
            )
            .await
            .unwrap_err();
        assert_eq!(err.code, "P2025");
        assert_eq!(counts(&client).await, before);
        client.db().database().drop().await.unwrap();
    }
}
