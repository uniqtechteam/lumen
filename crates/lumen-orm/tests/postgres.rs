//! SPEC-QUERY-001 FR-001..003, FR-007, FR-008 on a real Postgres: every filter (relation filters
//! too), ordering and
//! paging, inserts with casts, upserts, updates, deletes, counts, raw queries and coded errors.
//! Runs when `LUMEN_TEST_POSTGRES_URL` is set.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "tests: a failure should stop the test"
)]

mod common;

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};

use lumen_orm::sqlx::{AssertSqlSafe, FromRow, PgPool};
use lumen_orm::{
    Db, Delete, Filter, Insert, OnConflict, Order, Related, Select, Table, Update, Value, col,
};

const SCHEMA: &str = r#"
CREATE TYPE "Role" AS ENUM ('READER', 'WRITER', 'ADMIN');
CREATE TABLE "Item" (
    "id" SERIAL PRIMARY KEY,
    "name" TEXT NOT NULL,
    "note" TEXT,
    "code" UUID NOT NULL DEFAULT gen_random_uuid(),
    "price" NUMERIC(10,2) NOT NULL DEFAULT 0,
    "role" "Role" NOT NULL DEFAULT 'READER',
    "data" JSONB,
    "made" TIMESTAMP(3) NOT NULL DEFAULT CURRENT_TIMESTAMP,
    "tags" TEXT[] NOT NULL DEFAULT '{}',
    "active" BOOLEAN NOT NULL DEFAULT true,
    "parentId" INTEGER REFERENCES "Item"("id"),
    "tagIds" INTEGER[]
);
CREATE UNIQUE INDEX "Item_name_key" ON "Item"("name");
CREATE TABLE "Part" (
    "id" SERIAL PRIMARY KEY,
    "itemId" INTEGER NOT NULL REFERENCES "Item"("id"),
    "name" TEXT
);
CREATE TABLE "Tag" (
    "id" SERIAL PRIMARY KEY,
    "name" TEXT NOT NULL,
    "itemIds" INTEGER[] NOT NULL DEFAULT '{}'
);
CREATE TABLE "_ItemToTag" (
    "A" INTEGER NOT NULL REFERENCES "Item"("id"),
    "B" INTEGER NOT NULL REFERENCES "Tag"("id")
);
"#;

#[derive(Debug, FromRow, PartialEq)]
struct Id {
    id: i32,
}

#[derive(FromRow)]
struct RoleRow {
    role: String,
}

async fn db() -> Option<(PgPool, Db, String)> {
    static N: AtomicUsize = AtomicUsize::new(0);
    let url = std::env::var("LUMEN_TEST_POSTGRES_URL").ok()?;
    let admin = PgPool::connect(&url).await.unwrap();
    let name = format!(
        "lumen_orm_{}_{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    );
    sqlx_run(&admin, &format!("drop database if exists {name}")).await;
    sqlx_run(&admin, &format!("create database {name}")).await;
    let base = url.rsplit_once('/').map_or(url.as_str(), |(b, _)| b);
    let db = Db::connect(&format!("{base}/{name}")).await.unwrap();
    sqlx_run(db.pool(), SCHEMA).await;
    let db = db.with_constraints(HashMap::from([(
        "Item_name_key".to_owned(),
        vec!["name".to_owned()],
    )]));
    Some((admin, db, name))
}

async fn sqlx_run(pool: &PgPool, sql: &str) {
    lumen_orm::sqlx::raw_sql(AssertSqlSafe(sql.to_owned()))
        .execute(pool)
        .await
        .unwrap();
}

async fn finish(admin: PgPool, db: Db, name: String) {
    db.pool().close().await;
    sqlx_run(
        &admin,
        &format!("drop database if exists {name} with (force)"),
    )
    .await;
}

fn role(r: &str) -> Value {
    Value::Enum(r.to_owned(), "\"Role\"".to_owned())
}

async fn seed(db: &Db) {
    let rows = [
        ("Lamp", Some("warm light"), "12.50", "READER", vec!["home"]),
        ("lamp shade", None, "5.00", "WRITER", vec!["home", "deco"]),
        ("Desk", Some("100%_oak"), "250.00", "ADMIN", vec![]),
        (
            "Chair",
            Some("WARM seat"),
            "80.00",
            "WRITER",
            vec!["office"],
        ),
    ];
    let insert = Insert {
        table: Table::new("Item"),
        columns: vec![
            "name".into(),
            "note".into(),
            "price".into(),
            "role".into(),
            "tags".into(),
        ],
        rows: rows
            .iter()
            .map(|(n, note, price, r, tags)| {
                vec![
                    (*n).into(),
                    (*note).into(),
                    Value::Decimal((*price).to_owned()),
                    role(r),
                    Value::List(tags.iter().map(|t| Value::Text((*t).to_owned())).collect()),
                ]
            })
            .collect(),
        on_conflict: None,
        returning: vec!["id".into()],
    };
    let ids: Vec<Id> = db.fetch_all(insert.build()).await.unwrap();
    assert_eq!(
        ids,
        [Id { id: 1 }, Id { id: 2 }, Id { id: 3 }, Id { id: 4 }]
    );
}

async fn ids(db: &Db, filter: Filter) -> Vec<i32> {
    let mut q = Select::from(Table::new("Item"));
    q.columns = vec!["id".into()];
    q.filter = Some(filter);
    q.order = vec![("id".into(), Order::Asc)];
    db.fetch_all::<Id>(q.build())
        .await
        .unwrap()
        .into_iter()
        .map(|r| r.id)
        .collect()
}

/// Parts, parents and tags for the shared cases (`common`). Tags are kept both ways Postgres can
/// keep a many-to-many: the join table `_ItemToTag`, and id lists (`Item.tagIds`, `Tag.itemIds`).
async fn seed_relations(db: &Db) {
    let parts = Insert {
        table: Table::new("Part"),
        columns: vec!["itemId".into(), "name".into()],
        rows: common::PARTS
            .iter()
            .map(|(_, item, n)| vec![(*item).into(), (*n).into()])
            .collect(),
        on_conflict: None,
        returning: vec![],
    };
    db.execute(parts.build()).await.unwrap();
    for (child, parent) in common::PARENTS {
        let update = Update {
            table: Table::new("Item"),
            set: vec![("parentId".into(), parent.into())],
            filter: Some(col("id").equals(child)),
            returning: vec![],
        };
        db.execute(update.build()).await.unwrap();
    }
    let ids = |ids: Vec<i32>| Value::List(ids.into_iter().map(Value::Int).collect());
    let tags = Insert {
        table: Table::new("Tag"),
        columns: vec!["name".into(), "itemIds".into()],
        rows: common::TAGS
            .iter()
            .map(|(id, n)| vec![(*n).into(), ids(common::items_of(*id))])
            .collect(),
        on_conflict: None,
        returning: vec![],
    };
    db.execute(tags.build()).await.unwrap();
    let links = Insert {
        table: Table::new("_ItemToTag"),
        columns: vec!["A".into(), "B".into()],
        rows: common::ITEM_TAGS
            .iter()
            .map(|(item, tag)| vec![(*item).into(), (*tag).into()])
            .collect(),
        on_conflict: None,
        returning: vec![],
    };
    db.execute(links.build()).await.unwrap();
    for item in 1..=3 {
        let update = Update {
            table: Table::new("Item"),
            set: vec![("tagIds".into(), ids(common::tags_of(item)))],
            filter: Some(col("id").equals(item)),
            returning: vec![],
        };
        db.execute(update.build()).await.unwrap();
    }
}

#[tokio::test]
async fn ac_002_every_filter_returns_the_right_rows() {
    let Some((admin, db, name)) = db().await else {
        return;
    };
    seed(&db).await;
    seed_relations(&db).await;
    let through = |this: &str, other: &str, table: &str| {
        Related::through(
            Table::new(table),
            Table::new("_ItemToTag"),
            &[(this, "id")],
            &[(other, "id")],
        )
    };
    let join_table =
        common::many_to_many_cases(&|| through("A", "B", "Tag"), &|| through("B", "A", "Item"));
    let id_lists = common::many_to_many_cases(
        &|| Related::id_list(Table::new("Tag"), "tagIds", "id"),
        &|| Related::id_list(Table::new("Item"), "itemIds", "id"),
    );
    let cases: Vec<_> = common::field_cases(role)
        .into_iter()
        .chain(common::relation_cases())
        .chain(join_table)
        .chain(id_lists)
        .collect();
    assert_eq!(cases.len(), 59);
    for (what, filter, want) in cases {
        assert_eq!(ids(&db, filter).await, want, "{what}");
    }
    finish(admin, db, name).await;
}

#[tokio::test]
async fn ordering_paging_distinct_and_count() {
    let Some((admin, db, name)) = db().await else {
        return;
    };
    seed(&db).await;
    let mut q = Select::from(Table::new("Item"));
    q.columns = vec!["id".into()];
    q.order = vec![("price".into(), Order::Desc)];
    q.limit = Some(2);
    q.offset = Some(1);
    let page: Vec<i32> = db
        .fetch_all::<Id>(q.build())
        .await
        .unwrap()
        .into_iter()
        .map(|r| r.id)
        .collect();
    assert_eq!(page, [4, 1]);
    q.limit = None;
    q.offset = None;
    q.filter = Some(col("role").equals(role("WRITER")));
    assert_eq!(db.count(&q).await.unwrap(), 2);

    let mut d = Select::from(Table::new("Item"));
    d.columns = vec!["role".into()];
    d.distinct = vec!["role".into()];
    d.order = vec![("role".into(), Order::Asc)];
    let roles: Vec<String> = db
        .fetch_all::<RoleRow>(lumen_orm::Built {
            sql: d
                .build()
                .sql
                .replace("\"role\" FROM", "\"role\"::text AS role FROM"),
            params: d.build().params,
        })
        .await
        .unwrap()
        .into_iter()
        .map(|r| r.role)
        .collect();
    // ordered by the text alias, so alphabetically
    assert_eq!(roles, ["ADMIN", "READER", "WRITER"]);
    finish(admin, db, name).await;
}

#[tokio::test]
async fn updates_deletes_upserts_and_raw_queries() {
    let Some((admin, db, name)) = db().await else {
        return;
    };
    seed(&db).await;
    let update = Update {
        table: Table::new("Item"),
        set: vec![
            ("note".into(), Value::Null),
            ("active".into(), false.into()),
        ],
        filter: Some(col("role").equals(role("WRITER"))),
        returning: vec![],
    };
    assert_eq!(db.execute(update.build()).await.unwrap(), 2);
    assert_eq!(ids(&db, col("active").equals(false)).await, [2, 4]);

    let upsert = Insert {
        table: Table::new("Item"),
        columns: vec!["name".into(), "price".into()],
        rows: vec![vec!["Lamp".into(), Value::Decimal("9.99".into())]],
        on_conflict: Some(OnConflict {
            columns: vec!["name".into()],
            update: vec![("price".into(), Value::Decimal("9.99".into()))],
        }),
        returning: vec!["id".into()],
    };
    assert_eq!(
        db.fetch_one::<Id>(upsert.build()).await.unwrap(),
        Id { id: 1 },
        "the existing row is updated"
    );
    assert_eq!(
        ids(&db, col("price").equals(Value::Decimal("9.99".into()))).await,
        [1]
    );

    let delete = Delete {
        table: Table::new("Item"),
        filter: Some(col("id").equals(3)),
        returning: vec!["id".into()],
    };
    assert_eq!(
        db.fetch_all::<Id>(delete.build()).await.unwrap(),
        [Id { id: 3 }]
    );

    let raw: Vec<Id> = db
        .query_raw(
            "SELECT id FROM \"Item\" WHERE $1 = ANY(tags) ORDER BY id",
            vec!["home".into()],
        )
        .await
        .unwrap();
    assert_eq!(raw, [Id { id: 1 }, Id { id: 2 }]);
    let missing = db
        .fetch_one::<Id>(Select::from(Table::new("Item")).build().with_filter_none())
        .await;
    assert!(missing.is_ok());
    finish(admin, db, name).await;
}

trait NoFilter {
    fn with_filter_none(self) -> Self;
}

impl NoFilter for lumen_orm::Built {
    fn with_filter_none(self) -> Self {
        Self {
            sql: format!(
                "{} WHERE \"id\" = 1",
                self.sql.replace("SELECT *", "SELECT \"id\"")
            ),
            params: self.params,
        }
    }
}

#[tokio::test]
async fn ac_fr_008_errors_have_prisma_codes_and_fields() {
    let Some((admin, db, name)) = db().await else {
        return;
    };
    seed(&db).await;
    let duplicate = Insert {
        table: Table::new("Item"),
        columns: vec!["name".into()],
        rows: vec![vec!["Lamp".into()]],
        on_conflict: None,
        returning: vec![],
    };
    let e = db.execute(duplicate.build()).await.unwrap_err();
    assert_eq!(e.code, "P2002");
    assert_eq!(e.fields, ["name"]);
    assert_eq!(e.constraint.as_deref(), Some("Item_name_key"));
    assert_eq!(
        e.to_string(),
        "P2002: a record with the same name already exists"
    );

    let orphan = Insert {
        table: Table::new("Part"),
        columns: vec!["itemId".into()],
        rows: vec![vec![99.into()]],
        on_conflict: None,
        returning: vec![],
    };
    assert_eq!(db.execute(orphan.build()).await.unwrap_err().code, "P2003");

    let missing = Insert {
        table: Table::new("Item"),
        columns: vec!["note".into()],
        rows: vec![vec!["x".into()]],
        on_conflict: None,
        returning: vec![],
    };
    assert_eq!(db.execute(missing.build()).await.unwrap_err().code, "P2011");

    let nothing = Select {
        filter: Some(col("id").equals(999)),
        columns: vec!["id".into()],
        ..Select::from(Table::new("Item"))
    };
    assert_eq!(
        db.fetch_one::<Id>(nothing.build()).await.unwrap_err().code,
        "P2025"
    );
    finish(admin, db, name).await;
}
