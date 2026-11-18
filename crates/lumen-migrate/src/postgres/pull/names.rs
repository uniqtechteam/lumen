//! Names for what `db pull` reads (SPEC-CLI-001 FR-005, ADR-011): `PascalCase` models and enums,
//! camelCase fields, cleaned enum values, and a number when a name is taken.

use std::collections::HashSet;

/// Names a model or enum cannot have.
const RESERVED: [&str; 10] = [
    "String",
    "Int",
    "BigInt",
    "Float",
    "Decimal",
    "Boolean",
    "DateTime",
    "Json",
    "Bytes",
    "Unsupported",
];

/// The words of a database name: runs of letters and digits.
fn words(name: &str) -> Vec<&str> {
    name.split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect()
}

fn upper_first(word: &str) -> String {
    let mut c = word.chars();
    c.next().map_or_else(String::new, |f| {
        f.to_ascii_uppercase().to_string() + c.as_str()
    })
}

/// `URLPath` → `urlPath`, `ID` → `id`, `Email` → `email`.
fn lower_first(word: &str) -> String {
    let upper = word.chars().take_while(char::is_ascii_uppercase).count();
    if upper == word.len() {
        return word.to_ascii_lowercase();
    }
    let cut = if upper > 1 { upper - 1 } else { upper.max(1) };
    let (head, rest) = word.split_at(cut);
    head.to_ascii_lowercase() + rest
}

/// Whether `name` can be written as it is.
#[must_use]
pub fn is_valid(name: &str) -> bool {
    let mut c = name.chars();
    c.next().is_some_and(|f| f.is_ascii_alphabetic())
        && c.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// NR-1, NR-3: `order_items` → `OrderItems`, `2fa` → `M2fa`, `string` → `StringModel`.
#[must_use]
pub fn type_name(db: &str, prefix: char) -> String {
    let mut name: String = words(db).iter().map(|w| upper_first(w)).collect();
    if !name.starts_with(|c: char| c.is_ascii_alphabetic()) {
        name.insert(0, prefix);
    }
    if RESERVED.contains(&name.as_str()) {
        name.push_str("Model");
    }
    name
}

/// NR-1, NR-3: `created_at` → `createdAt`, `ID` → `id`, `1st` → `f1st`.
#[must_use]
pub fn field_name(db: &str) -> String {
    let w = words(db);
    let mut name = String::new();
    for (i, word) in w.iter().enumerate() {
        if i == 0 {
            name.push_str(&lower_first(word));
        } else {
            name.push_str(&upper_first(word));
        }
    }
    if !name.starts_with(|c: char| c.is_ascii_alphabetic()) {
        name.insert(0, 'f');
    }
    name
}

/// NR-1: an enum value is kept when it is a valid name, else cleaned: `in-progress` →
/// `in_progress`, `1` → `v1`, an empty value → `v`.
#[must_use]
pub fn value_name(db: &str) -> String {
    if is_valid(db) {
        return db.to_owned();
    }
    let mut name: String = db
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    if !name.starts_with(|c: char| c.is_ascii_alphabetic()) {
        name.insert(0, 'v');
    }
    name
}

/// `Post` → `posts`, `Category` → `categories`, `Box` → `boxes`, `News` → `news`.
#[must_use]
pub fn plural(name: &str) -> String {
    let lower = name.to_ascii_lowercase();
    if lower.ends_with('s') && !lower.ends_with("ss") {
        return name.to_owned();
    }
    if ["ss", "x", "z", "ch", "sh"]
        .iter()
        .any(|e| lower.ends_with(e))
    {
        return format!("{name}es");
    }
    let mut c = lower.chars().rev();
    if c.next() == Some('y') && c.next().is_some_and(|p| !"aeiou".contains(p)) {
        return format!("{}ies", &name[..name.len() - 1]);
    }
    format!("{name}s")
}

/// A model name as a field: `OrderItem` → `orderItem`.
#[must_use]
pub fn camel(model: &str) -> String {
    lower_first(model)
}

/// `name`, else `name2`, `name3`, .. : the first one not in `taken`, which it joins.
pub fn unique(name: String, taken: &mut HashSet<String>) -> String {
    if taken.insert(name.clone()) {
        return name;
    }
    let mut n = 2;
    loop {
        let candidate = format!("{name}{n}");
        if taken.insert(candidate.clone()) {
            return candidate;
        }
        n += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::{field_name, plural, type_name, value_name};

    #[test]
    fn naming_rules() {
        assert_eq!(type_name("order_items", 'M'), "OrderItems");
        assert_eq!(type_name("User", 'M'), "User");
        assert_eq!(type_name("userProfile", 'M'), "UserProfile");
        assert_eq!(type_name("2fa codes", 'M'), "M2faCodes");
        assert_eq!(type_name("string", 'M'), "StringModel");
        assert_eq!(field_name("created_at"), "createdAt");
        assert_eq!(field_name("createdAt"), "createdAt");
        assert_eq!(field_name("ID"), "id");
        assert_eq!(field_name("UserID"), "userID");
        assert_eq!(field_name("URLPath"), "urlPath");
        assert_eq!(field_name("Display Name"), "displayName");
        assert_eq!(field_name("1st"), "f1st");
        assert_eq!(value_name("in-progress"), "in_progress");
        assert_eq!(value_name("ACTIVE"), "ACTIVE");
        assert_eq!(value_name("1"), "v1");
        assert_eq!(plural("Post"), "Posts");
        assert_eq!(plural("category"), "categories");
        assert_eq!(plural("box"), "boxes");
        assert_eq!(plural("orderItems"), "orderItems");
        assert_eq!(plural("day"), "days");
    }
}
