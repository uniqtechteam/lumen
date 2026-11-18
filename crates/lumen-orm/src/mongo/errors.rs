//! Driver errors as coded ones. A duplicate key (E11000) is P2002 with the violated index in
//! `constraint` and, when the schema's names are known, its fields in `fields`, as on Postgres.
//! The driver keeps only the server's message for a write error (`keyPattern` is not exposed), so
//! the index name is read from the fixed part of that message: `... index: <name> dup key: ...`.

use std::collections::HashMap;

use mongodb::error::{ErrorKind, WriteFailure};

use crate::error::Error;

/// The message of the first duplicate-key write error in `e`, if it is one.
fn duplicate_message(e: &mongodb::error::Error) -> Option<&str> {
    match e.kind.as_ref() {
        ErrorKind::Write(WriteFailure::WriteError(w)) if w.code == 11000 => Some(&w.message),
        ErrorKind::InsertMany(m) => m
            .write_errors
            .as_ref()?
            .iter()
            .find(|w| w.code == 11000)
            .map(|w| w.message.as_str()),
        _ => None,
    }
}

/// The index named in a duplicate-key message.
fn index_of(message: &str) -> Option<&str> {
    let (_, rest) = message.split_once(" index: ")?;
    let (name, _) = rest.split_once(" dup key")?;
    Some(name.trim()).filter(|n| !n.is_empty())
}

/// A driver error as a coded [`Error`]: a duplicate is P2002 without fields, an error the driver
/// labels `TransientTransactionError` (a write conflict, say) is P2034, as a serialization failure
/// is on Postgres, and a server that cannot be reached is P1001.
pub(super) fn mongo_err(e: &mongodb::error::Error) -> Error {
    if let Some(message) = duplicate_message(e) {
        return Error::duplicate(index_of(message).map(str::to_owned), Vec::new());
    }
    let message = e.to_string();
    if e.contains_label(mongodb::error::TRANSIENT_TRANSACTION_ERROR) {
        // another transaction wrote the same document first (WriteConflict), or the connection
        // dropped before the commit: nothing was kept, so running it again is safe
        return Error::new(
            "P2034",
            format!("the transaction conflicted with another one; try again ({message})"),
        );
    }
    if matches!(
        e.kind.as_ref(),
        ErrorKind::ServerSelection { .. }
            | ErrorKind::Io(_)
            | ErrorKind::ConnectionPoolCleared { .. }
    ) {
        return Error::new("P1001", format!("cannot reach MongoDB: {message}"));
    }
    if message.contains("Transaction numbers are only allowed") || message.contains("replica set") {
        return Error::new(
            "P2010",
            format!("MongoDB transactions need a replica set or a sharded cluster ({message})"),
        );
    }
    Error::new("P2010", message)
}

/// A write error on `collection`: a duplicate names the fields of the violated index, looked up
/// as `"collection.index"` in `constraints`. Without an entry, the built-in `_id_` index names
/// `_id` and any other index leaves `fields` empty.
pub(super) fn write_err(
    e: &mongodb::error::Error,
    collection: &str,
    constraints: &HashMap<String, Vec<String>>,
) -> Error {
    let Some(index) = duplicate_message(e).and_then(index_of) else {
        return mongo_err(e);
    };
    let fields = constraints
        .get(&format!("{collection}.{index}"))
        .cloned()
        .unwrap_or_else(|| {
            if index == "_id_" {
                vec!["_id".to_owned()]
            } else {
                Vec::new()
            }
        });
    Error::duplicate(Some(index.to_owned()), fields)
}

#[cfg(test)]
mod tests {
    use super::index_of;

    #[test]
    fn the_index_is_read_from_a_duplicate_key_message() {
        let m = r#"E11000 duplicate key error collection: app.User index: User_email_key dup key: { email: "a index: b dup key" }"#;
        assert_eq!(index_of(m), Some("User_email_key"));
        assert_eq!(
            index_of(
                "E11000 duplicate key error collection: app.User index: _id_ dup key: { _id: 1 }"
            ),
            Some("_id_")
        );
        assert_eq!(index_of("E11000 duplicate key error"), None);
    }
}
