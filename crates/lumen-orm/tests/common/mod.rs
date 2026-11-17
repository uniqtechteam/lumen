//! The filter cases `tests/postgres.rs` and `tests/mongo.rs` both run, so the two databases answer
//! the same questions on the same data with the same expected rows.
//!
//! Items: 1 Lamp, 2 lamp shade, 3 Desk, 4 Chair (seeded by each file). Parts (`Part.itemId`): Lamp
//! has a bulb and a cable, the lamp shade a bulb, Chair a leg. Parents (`Item.parentId`): the lamp
//! shade's is Lamp, Chair's is Desk. Tags, many-to-many: Lamp has light and sale, the lamp shade
//! light, Desk wood, Chair none; the tag "unused" has no items.

use lumen_orm::{Filter, Related, Table, Value, col};

/// What a case is called, its filter on `Item`, and the item ids it must return, in order.
pub type Case = (&'static str, Filter, Vec<i32>);

/// `(part id, item id, name)`.
pub const PARTS: [(i32, i32, &str); 4] = [
    (1, 1, "bulb"),
    (2, 1, "cable"),
    (3, 2, "bulb"),
    (4, 4, "leg"),
];
/// `(child item, parent item)`.
pub const PARENTS: [(i32, i32); 2] = [(2, 1), (4, 3)];
/// `(tag id, name)`.
pub const TAGS: [(i32, &str); 4] = [(1, "light"), (2, "wood"), (3, "sale"), (4, "unused")];
/// `(item id, tag id)`: the many-to-many links.
pub const ITEM_TAGS: [(i32, i32); 4] = [(1, 1), (1, 3), (2, 1), (3, 2)];

/// The ids of the tags of `item`.
pub fn tags_of(item: i32) -> Vec<i32> {
    ITEM_TAGS
        .iter()
        .filter(|(i, _)| *i == item)
        .map(|(_, t)| *t)
        .collect()
}

/// The ids of the items tagged `tag`.
pub fn items_of(tag: i32) -> Vec<i32> {
    ITEM_TAGS
        .iter()
        .filter(|(_, t)| *t == tag)
        .map(|(i, _)| *i)
        .collect()
}

/// Field filters. `role` makes an enum value the way the database stores it.
pub fn field_cases(role: fn(&str) -> Value) -> Vec<Case> {
    let dec = |s: &str| Value::Decimal(s.to_owned());
    vec![
        ("equals", col("name").equals("Lamp"), vec![1]),
        (
            "equals, ignoring case",
            col("name").equals_insensitive("lamp"),
            vec![1],
        ),
        (
            "not equals keeps NULLs, like `not` on a nullable field",
            col("note").not_equals("warm light"),
            vec![2, 3, 4],
        ),
        ("in", col("id").is_in([1, 3]), vec![1, 3]),
        ("not in", col("id").not_in([1, 3]), vec![2, 4]),
        (
            "in an empty list matches nothing",
            col("id").is_in(Vec::<i32>::new()),
            vec![],
        ),
        ("lt on decimals", col("price").lt(dec("12.50")), vec![2]),
        ("lte", col("price").lte(dec("12.50")), vec![1, 2]),
        ("gt", col("price").gt(dec("80")), vec![3]),
        ("gte", col("price").gte(dec("80")), vec![3, 4]),
        ("contains", col("name").contains("amp"), vec![1, 2]),
        (
            "contains, ignoring case",
            col("note").contains_insensitive("warm"),
            vec![1, 4],
        ),
        (
            "contains escapes % and _ (and regex characters)",
            col("note").contains("0%_o"),
            vec![3],
        ),
        ("starts with", col("name").starts_with("lamp"), vec![2]),
        ("ends with", col("name").ends_with("k"), vec![3]),
        ("is null", col("note").is_null(), vec![2]),
        ("is not null", col("note").is_not_null(), vec![1, 3, 4]),
        ("enum", col("role").equals(role("WRITER")), vec![2, 4]),
        (
            "and",
            col("role")
                .equals(role("WRITER"))
                .and(col("note").is_null()),
            vec![2],
        ),
        (
            "or",
            col("id").equals(1).or(col("id").equals(4)),
            vec![1, 4],
        ),
        ("not", !col("id").lt(3), vec![3, 4]),
        (
            "an empty and is true",
            Filter::And(vec![]),
            vec![1, 2, 3, 4],
        ),
        ("an empty or is false", Filter::Or(vec![]), vec![]),
    ]
}

fn parts() -> Related {
    Related::new(Table::new("Part"), &[("itemId", "id")])
}

fn parent() -> Related {
    Related::new(Table::new("Item"), &[("id", "parentId")])
}

fn item() -> Related {
    Related::new(Table::new("Item"), &[("id", "itemId")])
}

/// Relation filters on one-to-many and to-one relations.
pub fn relation_cases() -> Vec<Case> {
    vec![
        (
            "starts with, ignoring case",
            col("name").starts_with_insensitive("LAMP"),
            vec![1, 2],
        ),
        (
            "ends with, ignoring case",
            col("name").ends_with_insensitive("SHADE"),
            vec![2],
        ),
        ("some", parts().some(col("name").equals("bulb")), vec![1, 2]),
        (
            "some with no condition: has any",
            parts().some(Filter::And(vec![])),
            vec![1, 2, 4],
        ),
        (
            "every is true for an item without parts",
            parts().every(col("name").equals("bulb")),
            vec![2, 3],
        ),
        (
            "every, ignoring case",
            parts().every(col("name").starts_with_insensitive("B")),
            vec![2, 3],
        ),
        ("none", parts().none(col("name").equals("bulb")), vec![3, 4]),
        (
            "is on an optional to-one",
            parent().is(col("name").equals("Lamp")),
            vec![2],
        ),
        (
            "is not keeps items without a parent",
            parent().is_not(col("name").equals("Lamp")),
            vec![1, 3, 4],
        ),
        ("is null", parent().is_null(), vec![1, 3]),
        ("is not null", parent().is_not_null(), vec![2, 4]),
        (
            "nested: a part whose item's parent is Desk",
            parts().some(item().is(parent().is(col("name").equals("Desk")))),
            vec![4],
        ),
        (
            "inside not",
            !parts().some(col("name").equals("bulb")),
            vec![3, 4],
        ),
        (
            "inside or and and",
            parent()
                .is_null()
                .and(parts().some(Filter::And(vec![])))
                .or(col("id").equals(4)),
            vec![1, 4],
        ),
    ]
}

/// Many-to-many filters. `tags` goes from an item to its tags, `items` from a tag to its items;
/// each database passes the way it keeps the relation (a join table, or id lists).
pub fn many_to_many_cases(tags: &dyn Fn() -> Related, items: &dyn Fn() -> Related) -> Vec<Case> {
    let any = || Filter::And(vec![]);
    vec![
        (
            "many-to-many some",
            tags().some(col("name").equals("light")),
            vec![1, 2],
        ),
        (
            "many-to-many some with no condition: has any",
            tags().some(any()),
            vec![1, 2, 3],
        ),
        (
            "many-to-many every is true for an item without tags",
            tags().every(col("name").equals("light")),
            vec![2, 4],
        ),
        (
            "many-to-many every with an in",
            tags().every(col("name").is_in(["light", "sale"])),
            vec![1, 2, 4],
        ),
        (
            "many-to-many none",
            tags().none(col("name").equals("light")),
            vec![3, 4],
        ),
        (
            "many-to-many none with no condition: has no tags",
            tags().none(any()),
            vec![4],
        ),
        (
            "many-to-many nested both ways: shares a tag with Desk",
            tags().some(items().some(col("name").equals("Desk"))),
            vec![3],
        ),
        (
            "many-to-many every over some: each tag is also on Lamp",
            tags().every(items().some(col("name").equals("Lamp"))),
            vec![1, 2, 4],
        ),
        (
            "many-to-many then one-to-many: shares a tag with an item that has a cable",
            tags().some(items().some(parts().some(col("name").equals("cable")))),
            vec![1, 2],
        ),
        (
            "many-to-many inside not",
            !tags().some(col("name").equals("sale")),
            vec![2, 3, 4],
        ),
        (
            "many-to-many and one-to-many together",
            tags()
                .some(col("name").equals("light"))
                .and(parts().none(col("name").equals("cable"))),
            vec![2],
        ),
    ]
}
