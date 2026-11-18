//! From a described database to schema blocks: models with their fields, keys and indexes,
//! relations, views and enums. On a second pull the file's names, order, comments and the
//! attributes that still describe the database are kept (SPEC-CLI-001 FR-005).

mod fields;
mod keys;
mod relations;

use std::collections::HashSet;

use super::Database;
use super::existing::{Around, Existing};
use super::names::{self, unique};
use crate::calculate::cut;
use crate::shape::{Action, Sort, Table};

/// One line of a block: a field or an enum value.
#[derive(Debug, Clone)]
pub struct Line {
    /// The name.
    pub name: String,
    /// The type with `?` or `[]`; empty for enum values.
    pub ty: String,
    /// The attributes, as text.
    pub attributes: Vec<String>,
    /// Comments written around it.
    pub around: Around,
    /// Its place in the file's block, when it was there.
    pub old: Option<usize>,
    /// A column (or value), as opposed to a relation field.
    pub column: bool,
}

impl Line {
    fn new(name: String, ty: String, attributes: Vec<String>, column: bool) -> Self {
        Self {
            name,
            ty,
            attributes,
            around: Around::default(),
            old: None,
            column,
        }
    }
}

/// A `@@` attribute line.
#[derive(Debug, Clone)]
pub struct BlockAttribute {
    /// The text.
    pub text: String,
    /// Comments written around it.
    pub around: Around,
}

/// One block of the new schema.
#[derive(Debug, Clone)]
pub struct Block {
    /// `model`, `view` or `enum`.
    pub keyword: &'static str,
    /// The name.
    pub name: String,
    /// Fields or values.
    pub lines: Vec<Line>,
    /// `@@` attributes.
    pub attributes: Vec<BlockAttribute>,
    /// lumen's notes about what the schema cannot say, written above the block.
    pub notes: Vec<String>,
    /// Things worth saying that the block shows already (`Unsupported`, `@@ignore`); printed only.
    pub reports: Vec<String>,
    /// The comments above the block in the file.
    pub above: String,
    /// The comments before its `}` in the file.
    pub tail: String,
    /// Its top-level place in the file, when it was there.
    pub old: Option<usize>,
}

impl Block {
    fn new(keyword: &'static str, name: String) -> Self {
        Self {
            keyword,
            name,
            lines: Vec::new(),
            attributes: Vec::new(),
            notes: Vec::new(),
            reports: Vec::new(),
            above: String::new(),
            tail: String::new(),
            old: None,
        }
    }

    fn attribute(&mut self, text: String) {
        self.attributes.push(BlockAttribute {
            text,
            around: Around::default(),
        });
    }

    /// The `@@` attributes of the file's block that the database cannot know, kept as written;
    /// `skip` are the ones the pull writes itself.
    fn keep_attributes(
        &mut self,
        x: &Existing<'_>,
        attributes: &[lumen_schema::ast::Attribute],
        around: &[Around],
        skip: &[&str],
    ) {
        for (attr, around) in attributes.iter().zip(around) {
            if !skip.contains(&attr.name.name.as_str()) {
                self.attributes.push(BlockAttribute {
                    text: x.source(attr.span).to_owned(),
                    around: around.clone(),
                });
            }
        }
    }
}

/// What the pull built.
pub struct Built {
    /// The blocks: models, views, then enums.
    pub blocks: Vec<Block>,
    /// Notes for the whole database (things left out).
    pub notes: Vec<String>,
}

struct PulledEnum {
    name: String,
    /// The type as `format_type` prints it, with and without the schema.
    printed: Vec<String>,
    /// (database name, schema name) of each value.
    values: Vec<(String, String)>,
}

/// A table that becomes a model.
struct PulledModel<'a> {
    table: &'a Table,
    name: String,
    /// The file's model for the table.
    old: Option<usize>,
    /// The field name of each column.
    fields: Vec<String>,
    /// Field names in use.
    taken: HashSet<String>,
    block: Block,
}

/// A join table that becomes a many-to-many relation.
struct Join {
    /// The relation name: the table name without `_`.
    name: String,
    a: usize,
    b: usize,
}

struct Builder<'a> {
    db: &'a Database,
    old: Option<&'a Existing<'a>>,
    datasource: String,
    multi_schema: bool,
    enums: Vec<PulledEnum>,
    models: Vec<PulledModel<'a>>,
    notes: Vec<String>,
}

fn list(names: &[String]) -> String {
    format!("[{}]", names.join(", "))
}

fn upper_first(s: &str) -> String {
    let mut c = s.chars();
    c.next().map_or_else(String::new, |f| {
        f.to_ascii_uppercase().to_string() + c.as_str()
    })
}

/// Whether `t` is exactly the join table lumen writes for a many-to-many relation; the tables
/// `A` and `B` point at, by position in `tables`.
fn join_shape(t: &Table, tables: &[Table]) -> Option<(usize, usize)> {
    let name = t.name.strip_prefix('_').filter(|n| !n.is_empty())?;
    let [a, b] = t.columns.as_slice() else {
        return None;
    };
    let plain =
        |c: &crate::shape::Column, n: &str| c.name == n && !c.nullable && c.default.is_none();
    let pk = t.primary_key.as_ref()?;
    let [index] = t.indexes.as_slice() else {
        return None;
    };
    let index_ok = !index.unique
        && index.method.is_none()
        && index.predicate.is_none()
        && index.name == cut(&format!("_{name}_B"), "index")
        && index.columns.len() == 1
        && index.columns[0].name == "B"
        && index.columns[0].sort == Sort::Asc;
    if !plain(a, "A")
        || !plain(b, "B")
        || pk.columns != ["A", "B"]
        || pk.name != cut(&format!("_{name}_AB"), "pkey")
        || !index_ok
        || t.foreign_keys.len() != 2
    {
        return None;
    }
    let target = |column: &str| -> Option<usize> {
        let fk = t.foreign_keys.iter().find(|f| f.columns == [column])?;
        let ok = fk.name == cut(&format!("_{name}_{column}"), "fkey")
            && fk.on_delete == Action::Cascade
            && fk.on_update == Action::Cascade
            && fk.referenced_schema == t.schema;
        let i = tables
            .iter()
            .position(|x| x.schema == fk.referenced_schema && x.name == fk.referenced_table)?;
        let target_pk = tables[i].primary_key.as_ref()?;
        let col = tables[i].column(&fk.referenced_columns[0])?;
        let ty = &t.column(column)?.ty;
        (ok && target_pk.columns == fk.referenced_columns && col.ty == *ty).then_some(i)
    };
    Some((target("A")?, target("B")?))
}

/// Whether the columns `set` of `t` are a key: the primary key or a full unique index.
fn is_key(t: &Table, set: &[String]) -> bool {
    fn sorted(mut c: Vec<&String>) -> Vec<&String> {
        c.sort();
        c
    }
    let want = sorted(set.iter().collect());
    t.primary_key
        .as_ref()
        .is_some_and(|pk| sorted(pk.columns.iter().collect()) == want)
        || t.indexes.iter().any(|i| {
            i.unique
                && i.predicate.is_none()
                && !i.expression
                && sorted(i.columns.iter().map(|c| &c.name).collect()) == want
        })
}

impl Builder<'_> {
    fn schema_attribute(&self, block: &mut Block, schema: &str) {
        if self.multi_schema {
            block.attribute(format!("@@schema({})", super::types::string(schema)));
        }
    }

    /// NR-1 to NR-3 for a model, view or enum: the file's name, else a new one.
    fn type_name(
        &self,
        taken: &mut HashSet<String>,
        old: Option<String>,
        schema: &str,
        db: &str,
    ) -> String {
        if let Some(name) = old {
            return name;
        }
        let first_schema = self.db.schemas.first().map_or("public", String::as_str);
        let mut name = names::type_name(db, 'M');
        if taken.contains(&name) && schema != first_schema {
            name = format!("{}{name}", names::type_name(schema, 'M'));
        }
        unique(name, taken)
    }

    fn model_of(&self, schema: &str, table: &str) -> Option<usize> {
        self.models
            .iter()
            .position(|m| m.table.schema == schema && m.table.name == table)
    }
}

impl<'a> Builder<'a> {
    fn push_model(&mut self, table: &'a Table, name: String, old: Option<usize>) -> usize {
        self.models.push(PulledModel {
            table,
            name: name.clone(),
            old,
            fields: Vec::new(),
            taken: HashSet::new(),
            block: Block::new("model", name),
        });
        self.models.len() - 1
    }
}

/// Build the blocks for `db`, keeping what `old` says where the database agrees.
#[must_use]
pub fn build(
    db: &Database,
    old: Option<&Existing<'_>>,
    datasource: &str,
    multi_schema: bool,
) -> Built {
    let mut b = Builder {
        db,
        old,
        datasource: datasource.to_owned(),
        multi_schema,
        enums: Vec::new(),
        models: Vec::new(),
        notes: Vec::new(),
    };
    let tables = &db.shape.tables;
    let old_model =
        |schema: &str, name: &str, view: bool| old.and_then(|x| x.model(schema, name, view));
    let old_name = |o: Option<usize>| o.and_then(|i| old.map(|x| x.checked(i).name.clone()));
    let model_olds: Vec<Option<usize>> = tables
        .iter()
        .map(|t| old_model(&t.schema, &t.name, false))
        .collect();
    let view_olds: Vec<Option<usize>> = db
        .views
        .iter()
        .map(|v| old_model(&v.schema, &v.name, true))
        .collect();
    // the file's names are taken first, so a new block never takes one
    let mut taken: HashSet<String> = model_olds
        .iter()
        .chain(&view_olds)
        .filter_map(|o| old_name(*o))
        .collect();
    if let Some(x) = old {
        for e in &db.shape.enums {
            if let Some(i) = x.enumeration(&e.schema, &e.name) {
                taken.insert(x.schema.enums[i].name.clone());
            }
        }
    }
    let enums = b.enums(&mut taken);
    // join tables wait until the models they join have names
    let joins: Vec<Option<(usize, usize)>> = tables
        .iter()
        .zip(&model_olds)
        .map(|(t, o)| {
            if o.is_some() {
                None
            } else {
                join_shape(t, tables)
            }
        })
        .collect();
    let mut model_of_table: Vec<Option<usize>> = vec![None; tables.len()];
    for (ti, t) in tables.iter().enumerate() {
        if joins[ti].is_none() {
            let name = b.type_name(&mut taken, old_name(model_olds[ti]), &t.schema, &t.name);
            model_of_table[ti] = Some(b.push_model(t, name, model_olds[ti]));
        }
    }
    // a join table is a relation only when `A` points at the first model by name
    let mut join_list = Vec::new();
    for (ti, t) in tables.iter().enumerate() {
        let Some((a, bt)) = joins[ti] else { continue };
        if let (Some(ma), Some(mb)) = (model_of_table[a], model_of_table[bt])
            && b.models[ma].name <= b.models[mb].name
        {
            join_list.push(Join {
                name: t.name[1..].to_owned(),
                a: ma,
                b: mb,
            });
        } else {
            let name = b.type_name(&mut taken, None, &t.schema, &t.name);
            b.push_model(t, name, None);
        }
    }
    for m in 0..b.models.len() {
        b.columns(m);
        b.keys(m);
    }
    b.relations(&join_list);
    let mut blocks: Vec<Block> = b
        .models
        .iter_mut()
        .map(|m| std::mem::replace(&mut m.block, Block::new("model", String::new())))
        .collect();
    for (v, o) in db.views.iter().zip(&view_olds) {
        let name = b.type_name(&mut taken, old_name(*o), &v.schema, &v.name);
        blocks.push(b.view(v, &name, *o));
    }
    blocks.extend(enums);
    for block in &mut blocks {
        arrange(block);
    }
    Built {
        blocks,
        notes: b.notes,
    }
}

/// The file's lines keep their order; a new column goes after the last column, a new relation
/// field at the end.
fn arrange(block: &mut Block) {
    let lines = std::mem::take(&mut block.lines);
    let (mut out, new): (Vec<Line>, Vec<Line>) = lines.into_iter().partition(|l| l.old.is_some());
    out.sort_by_key(|l| l.old);
    let (columns, relations): (Vec<Line>, Vec<Line>) = new.into_iter().partition(|l| l.column);
    let at = out.iter().rposition(|l| l.column).map_or(0, |p| p + 1);
    let after = out.split_off(at);
    out.extend(columns);
    out.extend(after);
    out.extend(relations);
    block.lines = out;
}
