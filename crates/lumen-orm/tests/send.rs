//! Futures that use the pool or a transaction are `Send`, so they work in a web handler or
//! `tokio::spawn`. Frameworks check this generically (`F: Fn(..) -> Fut, Fut: Send`), which is
//! where a transaction taking `&mut Tx<'static>` used to fail to compile with "implementation of
//! `AsyncFnOnce` is not general enough". Nothing here runs; it only has to compile.

use std::future::Future;

use lumen_orm::mongo::{MongoDb, MongoRun};
use lumen_orm::{Db, Isolation, Run, Select, Table};

fn handler<A, F, Fut>(_f: F)
where
    F: Fn(A) -> Fut,
    Fut: Future + Send,
{
}

async fn on_the_pool(db: Db) -> lumen_orm::Result<i64> {
    db.count(&Select::from(Table::new("users"))).await
}

async fn in_a_transaction(db: Db) -> lumen_orm::Result<i64> {
    let name = String::from("users");
    db.transaction(async |tx| tx.count(&Select::from(Table::new(&name))).await)
        .await
}

async fn with_isolation(db: Db) -> lumen_orm::Result<i64> {
    db.transaction_with(Isolation::Serializable, async |tx| {
        tx.transaction(async |inner| inner.count(&Select::from(Table::new("users"))).await)
            .await
    })
    .await
}

async fn by_hand(db: Db) -> lumen_orm::Result<i64> {
    let mut tx = db.begin().await?;
    let n = tx.count(&Select::from(Table::new("users"))).await?;
    tx.commit().await?;
    Ok(n)
}

async fn on_mongo(db: MongoDb) -> lumen_orm::Result<u64> {
    db.transaction(async |tx| tx.count("users", None).await)
        .await
}

#[test]
fn ac_001_futures_are_send_in_generic_handlers() {
    handler(on_the_pool);
    handler(in_a_transaction);
    handler(with_isolation);
    handler(by_hand);
    handler(on_mongo);
}
