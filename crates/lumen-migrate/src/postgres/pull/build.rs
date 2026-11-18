//! From a described database to schema blocks: models with their fields, keys and indexes,
//! views and enums. On a second pull the file's names, order, comments and the attributes that
//! still describe the database are kept (SPEC-CLI-001 FR-005).

use std::collections::{HashMap, HashSet};

use lumen_schema::checked::IndexKind;

use super::existing::{Around, Existing};
use super::names::{self, unique};
use super::types::{self, ColumnKind, Kind};
use super::{Database, View};
use crate::calculate::cut;
use crate::postgres::types::type_name;
use crate::shape::{Action, EnumType, ForeignKey, Index, Sort, Table};

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
    /// lumen's notes about what the schema cannot say.
    pub notes: Vec<String>,
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
}

/// What the pull built.
pub struct Built {
    /// The blocks, models first, then views, then enums.
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
    old: Option<usize>,
    /// Field name of each column.
    fields: Vec<String>,
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

fn action(a: Action) -> &'static str {
    match a {
        Action::Cascade => "Cascade",
        Action::Restrict => "Restrict",
        Action::NoAction => "NoAction",
        Action::SetNull => "SetNull",
        Action::SetDefault => "SetDefault",
    }
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

/// The column type of each enum, as `format_type` may print it.
fn printed(e: &EnumType) -> Vec<String> {
    let bare = type_name(&e.name);
    vec![format!("{}.{bare}", type_name(&e.schema)), bare]
}

/// Whether `t` is exactly the join table lumen writes for a many-to-many relation; the two
/// tables `A` and `B` point at, by position in `tables`.
fn join_shape(t: &Table, tables: &[Table]) -> Option<(usize, usize)> {
    let name = t.name.strip_prefix('_').filter(|n| !n.is_empty())?;
    let [a, b] = t.columns.as_slice() else {
        return None;
    };
    if a.name != "A" || b.name != "B" || a.nullable || b.nullable {
        return None;
    }
    if a.default.is_some() || b.default.is_some() {
        return None;
    }
    let pk = t.primary_key.as_ref()?;
    if pk.columns != ["A", "B"] || pk.name != cut(&format!("_{name}_AB"), "pkey") {
        return None;
    }
    let [index] = t.indexes.as_slice() else {
        return None;
    };
    if index.unique
        || index.method.is_some()
        || index.predicate.is_some()
        || index.name != cut(&format!("_{name}_B"), "index")
        || index.columns.len() != 1
        || index.columns[0].name != "B"
        || index.columns[0].sort != Sort::Asc
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
    if t.foreign_keys.len() != 2 {
        return None;
    }
    Some((target("A")?, target("B")?))
}

/// Whether the columns `set` of `t` are a key: the primary key or a full unique index.
fn is_key(t: &Table, set: &[String]) -> bool {
    let same = |cols: &mut dyn Iterator<Item = &String>| {
        let mut a: Vec<&String> = cols.collect();
        let mut b: Vec<&String> = set.iter().collect();
        a.sort();
        b.sort();
        a == b
    };
    t.primary_key
        .as_ref()
        .is_some_and(|pk| same(&mut pk.columns.iter()))
        || t.indexes.iter().any(|i| {
            i.unique
                && i.predicate.is_none()
                && !i.expression
                && same(&mut i.columns.iter().map(|c| &c.name))
        })
}

impl<'a> Builder<'a> {
    fn schema_attribute(&self, block: &mut Block, schema: &str) {
        if self.multi_schema {
            block.attribute(format!("@@schema({})", types::string(schema)));
        }
    }

    /// NR-1 to NR-3 for models, views and enums: the file's names first, then new ones.
    fn type_names(
        &self,
        taken: &mut HashSet<String>,
        wanted: Vec<(Option<String>, String, String)>,
    ) -> Vec<String> {
        let first_schema = self.db.schemas.first().map_or("public", String::as_str);
        wanted
            .into_iter()
            .map(|(old, schema, db)| match old {
                Some(name) => name,
                None => {
                    let mut name = names::type_name(&db, 'M');
                    if taken.contains(&name) && schema != first_schema {
                        name = format!("{}{name}", names::type_name(&schema, 'M'));
                    }
                    unique(name, taken)
                }
            })
            .collect()
    }

    fn enums(&mut self, taken: &mut HashSet<String>) -> Vec<Block> {
        let olds: Vec<Option<usize>> = self
            .db
            .shape
            .enums
            .iter()
            .map(|e| self.old.and_then(|o| o.enumeration(&e.schema, &e.name)))
            .collect();
        let wanted = self
            .db
            .shape
            .enums
            .iter()
            .zip(&olds)
            .map(|(e, o)| {
                let old_name = o.and_then(|i| self.old.map(|x| x.schema.enums[i].name.clone()));
                (old_name, e.schema.clone(), e.name.clone())
            })
            .collect();
        let names = self.type_names(taken, wanted);
        let mut blocks = Vec::new();
        for ((e, name), old) in self.db.shape.enums.iter().zip(names).zip(olds) {
            let mut block = Block::new("enum", name.clone());
            let mut values = Vec::new();
            let mut taken_values = HashSet::new();
            let old_enum = old.and_then(|i| self.old.map(|x| (x, i)));
            if let Some((x, i)) = old_enum {
                let ast = x.enum_block(i);
                let mut spans: Vec<_> = ast.values.iter().map(|v| v.span).collect();
                spans.extend(ast.attributes.iter().map(|a| a.span));
                let (around, tail) = x.around(ast.name.span, &spans, ast.span);
                block.above = x.above_top(x.top_of_enum(i));
                block.tail = tail;
                block.old = Some(x.top_of_enum(i));
                for v in &x.schema.enums[i].values {
                    taken_values.insert(v.name.clone());
                }
                for (k, attr) in ast.attributes.iter().enumerate() {
                    let n = attr.name.name.as_str();
                    if n != "map" && n != "schema" {
                        block.attributes.push(BlockAttribute {
                            text: x.source(attr.span).to_owned(),
                            around: around[ast.values.len() + k].clone(),
                        });
                    }
                }
                for db_value in &e.values {
                    let found = x.schema.enums[i]
                        .values
                        .iter()
                        .position(|v| v.mapped_name.as_deref().unwrap_or(&v.name) == db_value);
                    let value_name = found.map_or_else(
                        || unique(names::value_name(db_value), &mut taken_values),
                        |k| x.schema.enums[i].values[k].name.clone(),
                    );
                    let mut attributes = Vec::new();
                    if let Some(k) = found {
                        for a in &ast.values[k].attributes {
                            if a.name.name != "map" {
                                attributes.push(x.source(a.span).to_owned());
                            }
                        }
                    }
                    if value_name != *db_value {
                        attributes.push(format!("@map({})", types::string(db_value)));
                    }
                    values.push((db_value.clone(), value_name.clone()));
                    block.lines.push(Line {
                        name: value_name,
                        ty: String::new(),
                        attributes,
                        around: found.map(|k| around[k].clone()).unwrap_or_default(),
                        old: found,
                        column: true,
                    });
                }
            } else {
                for db_value in &e.values {
                    let value_name = unique(names::value_name(db_value), &mut taken_values);
                    let mut attributes = Vec::new();
                    if value_name != *db_value {
                        attributes.push(format!("@map({})", types::string(db_value)));
                    }
                    values.push((db_value.clone(), value_name.clone()));
                    block.lines.push(Line {
                        name: value_name,
                        ty: String::new(),
                        attributes,
                        around: Around::default(),
                        old: None,
                        column: true,
                    });
                }
            }
            if name != e.name {
                block.attribute(format!("@@map({})", types::string(&e.name)));
            }
            self.schema_attribute(&mut block, &e.schema);
            self.enums.push(PulledEnum {
                name,
                printed: printed(e),
                values,
            });
            blocks.push(block);
        }
        blocks
    }

    fn type_text(&self, kind: &ColumnKind, nullable: bool) -> String {
        let base = match &kind.kind {
            Kind::Scalar(s, _) => s.name().to_owned(),
            Kind::Enum(i) => self.enums[*i].name.clone(),
            Kind::Unsupported(raw) => format!("Unsupported({})", types::string(raw)),
        };
        if kind.list {
            format!("{base}[]")
        } else if nullable {
            format!("{base}?")
        } else {
            base
        }
    }

    fn printed_enums(&self) -> Vec<Vec<String>> {
        self.enums.iter().map(|e| e.printed.clone()).collect()
    }

    /// The column fields of model `m`, with what the file said about them kept where the
    /// database agrees.
    fn columns(&mut self, m: usize) {
        let printed = self.printed_enums();
        let table = self.models[m].table;
        let old = self.models[m].old.and_then(|i| self.old.map(|x| (x, i)));
        let mut around = Vec::new();
        if let Some((x, i)) = old {
            let ast = x.block(i);
            let mut spans: Vec<_> = ast.fields.iter().map(|f| f.span).collect();
            spans.extend(ast.attributes.iter().map(|a| a.span));
            let (a, tail) = x.around(ast.name.span, &spans, ast.span);
            around = a;
            let block = &mut self.models[m].block;
            block.above = x.above_top(x.top_of_model(i));
            block.tail = tail;
            block.old = Some(x.top_of_model(i));
            // every name the file uses stays taken, relation fields included
            for f in &x.checked(i).fields {
                self.models[m].taken.insert(f.name.clone());
            }
        }
        let mut fields = Vec::new();
        for column in &table.columns {
            let kind = types::column_kind(&column.ty, &printed);
            let found = old.and_then(|(x, i)| x.field(i, &column.name).map(|f| (x, i, f)));
            let name = match found {
                Some((x, i, f)) => x.checked(i).fields[f].name.clone(),
                None => unique(names::field_name(&column.name), &mut self.models[m].taken),
            };
            let values = match kind.kind {
                Kind::Enum(e) => self.enums[e].values.clone(),
                _ => Vec::new(),
            };
            let mut ty = self.type_text(&kind, column.nullable);
            let mut native = match &kind.kind {
                Kind::Scalar(_, Some(n)) => Some(types::native_attribute(&self.datasource, n)),
                _ => None,
            };
            let mut default = column
                .default
                .as_ref()
                .and_then(|d| types::default(&kind, d, &values));
            let mut kept = Vec::new();
            let mut line_around = Around::default();
            if let Some((x, i, f)) = found {
                let ast = &x.block(i).fields[f];
                line_around = around[f].clone();
                let old_column = x.table(i).and_then(|t| t.column(&column.name));
                if old_column.is_some_and(|c| c.ty == column.ty && c.nullable == column.nullable)
                    && let Some(t) = &ast.ty
                {
                    ty = x.source(t.span).to_owned();
                    native = None;
                    kept.extend(
                        ast.attributes
                            .iter()
                            .filter(|a| a.name.name.contains('.'))
                            .map(|a| x.source(a.span).to_owned()),
                    );
                }
                if old_column.is_some_and(|c| c.default == column.default) {
                    default = ast
                        .attributes
                        .iter()
                        .find(|a| a.name.name == "default")
                        .map(|a| x.source(a.span).to_owned());
                }
                kept.extend(
                    ast.attributes
                        .iter()
                        .filter(|a| {
                            let n = a.name.name.as_str();
                            !matches!(n, "id" | "unique" | "default" | "map" | "relation")
                                && !n.contains('.')
                        })
                        .map(|a| x.source(a.span).to_owned()),
                );
            }
            let mut attributes: Vec<String> = default.into_iter().collect();
            if name != column.name {
                attributes.push(format!("@map({})", types::string(&column.name)));
            }
            attributes.extend(native);
            attributes.extend(kept);
            if let Kind::Unsupported(raw) = &kind.kind {
                self.models[m].block.notes.push(format!(
                    "`{name}` has the type `{raw}`, which the client cannot read (Unsupported)"
                ));
            }
            if kind.list && !column.nullable {
                self.models[m].block.notes.push(format!(
                    "`{name}` is NOT NULL in the database; list fields allow NULL, so `lumen migrate dev` would drop the constraint"
                ));
            }
            fields.push(name.clone());
            self.models[m].block.lines.push(Line {
                name,
                ty,
                attributes,
                around: line_around,
                old: found.map(|(_, _, f)| f),
                column: true,
            });
        }
        self.models[m].fields = fields;
        if let Some((x, i)) = old {
            let ast = x.block(i);
            for (k, attr) in ast.attributes.iter().enumerate() {
                let n = attr.name.name.as_str();
                if !matches!(n, "id" | "unique" | "index" | "map" | "schema") {
                    self.models[m].block.attributes.push(BlockAttribute {
                        text: x.source(attr.span).to_owned(),
                        around: around[ast.fields.len() + k].clone(),
                    });
                }
            }
        }
    }

    /// The file's key and index attributes that still describe the database, kept as written;
    /// the names of the database keys they cover.
    fn kept_keys(&mut self, m: usize) -> HashSet<String> {
        let mut covered = HashSet::new();
        let Some((x, i)) = self.models[m].old.and_then(|i| self.old.map(|x| (x, i))) else {
            return covered;
        };
        let table = self.models[m].table;
        let Some(want) = x.table(i) else {
            return covered;
        };
        let ast = x.block(i);
        let mut spans: Vec<_> = ast.fields.iter().map(|f| f.span).collect();
        spans.extend(ast.attributes.iter().map(|a| a.span));
        let (around, _) = x.around(ast.name.span, &spans, ast.span);
        for index in &x.checked(i).indexes {
            let cols: Vec<String> = index
                .fields
                .iter()
                .map(|f| x.checked(i).fields[f.field].database_name().to_owned())
                .collect();
            let (same, name) = if index.kind == IndexKind::PrimaryKey {
                let name = index
                    .mapped_name
                    .clone()
                    .unwrap_or_else(|| cut(&table.name, "pkey"));
                (
                    table.primary_key.is_some() && table.primary_key == want.primary_key,
                    name,
                )
            } else {
                let suffix = if index.kind == IndexKind::Unique {
                    "key"
                } else {
                    "idx"
                };
                let name = index
                    .mapped_name
                    .clone()
                    .unwrap_or_else(|| cut(&format!("{}_{}", table.name, cols.join("_")), suffix));
                let db = table.indexes.iter().find(|j| j.name == name);
                let w = want.indexes.iter().find(|j| j.name == name);
                (db.is_some() && db == w, name)
            };
            if !same {
                continue;
            }
            covered.insert(name);
            // the attribute that made it: on a field, or in the block
            let text = x.source(index.span).to_owned();
            if let Some(f) = ast
                .fields
                .iter()
                .position(|f| f.attributes.iter().any(|a| a.span == index.span))
            {
                if let Some(line) = self.models[m]
                    .block
                    .lines
                    .iter_mut()
                    .find(|l| l.old == Some(f) && l.column)
                {
                    line.attributes.push(text);
                }
            } else if let Some(k) = ast.attributes.iter().position(|a| a.span == index.span) {
                self.models[m].block.attributes.push(BlockAttribute {
                    text,
                    around: around[ast.fields.len() + k].clone(),
                });
            }
        }
        covered
    }

    fn field_list(&self, m: usize, columns: &[String]) -> Vec<String> {
        let t = self.models[m].table;
        columns
            .iter()
            .map(|c| {
                t.columns
                    .iter()
                    .position(|x| x.name == *c)
                    .map_or_else(|| c.clone(), |k| self.models[m].fields[k].clone())
            })
            .collect()
    }

    fn line_of(&mut self, m: usize, column: &str) -> Option<&mut Line> {
        let k = self.models[m]
            .table
            .columns
            .iter()
            .position(|c| c.name == column)?;
        let name = self.models[m].fields[k].clone();
        self.models[m]
            .block
            .lines
            .iter_mut()
            .find(|l| l.column && l.name == name)
    }

    fn index_args(&self, m: usize, i: &Index, default_name: &str) -> String {
        let names = self.field_list(
            m,
            &i.columns.iter().map(|c| c.name.clone()).collect::<Vec<_>>(),
        );
        let fields: Vec<String> = names
            .into_iter()
            .zip(&i.columns)
            .map(|(n, c)| {
                if c.sort == Sort::Desc {
                    format!("{n}(sort: Desc)")
                } else {
                    n
                }
            })
            .collect();
        let mut args = vec![list(&fields)];
        if i.name != default_name {
            args.push(format!("map: {}", types::string(&i.name)));
        }
        if let Some(method) = &i.method {
            let kind = match method.as_str() {
                "hash" => "Hash",
                "gin" => "Gin",
                "gist" => "Gist",
                "brin" => "Brin",
                "spgist" => "SpGist",
                other => other,
            };
            args.push(format!("type: {kind}"));
        }
        if let Some(p) = &i.predicate {
            args.push(format!("where: raw({})", types::string(p)));
        }
        args.join(", ")
    }

    /// `@id`, `@unique`, `@@id`, `@@unique`, `@@index`, `@@ignore`, for the database keys the
    /// file does not already say.
    fn keys(&mut self, m: usize) {
        let covered = self.kept_keys(m);
        let table = self.models[m].table;
        let model_name = self.models[m].name.clone();
        if let Some(pk) = &table.primary_key
            && !covered.contains(&pk.name)
        {
            let default = cut(&table.name, "pkey");
            let map = if pk.name == default {
                String::new()
            } else {
                format!("map: {}", types::string(&pk.name))
            };
            if let [column] = pk.columns.as_slice() {
                let text = if map.is_empty() {
                    "@id".to_owned()
                } else {
                    format!("@id({map})")
                };
                if let Some(line) = self.line_of(m, column) {
                    line.attributes.insert(0, text);
                }
            } else {
                let fields = list(&self.field_list(m, &pk.columns));
                let text = if map.is_empty() {
                    format!("@@id({fields})")
                } else {
                    format!("@@id({fields}, {map})")
                };
                self.models[m].block.attribute(text);
            }
        }
        for i in &table.indexes {
            if covered.contains(&i.name) {
                continue;
            }
            if i.expression {
                let cols: Vec<&str> = i.columns.iter().map(|c| c.name.as_str()).collect();
                self.models[m].block.notes.push(format!(
                    "the index `{}` is on an expression ({}); the schema cannot say it, so `lumen migrate dev` would drop it",
                    i.name,
                    cols.join(", ")
                ));
                continue;
            }
            let cols: Vec<String> = i.columns.iter().map(|c| c.name.clone()).collect();
            let suffix = if i.unique { "key" } else { "idx" };
            let default = cut(&format!("{}_{}", table.name, cols.join("_")), suffix);
            let single = i.unique
                && i.columns.len() == 1
                && i.columns[0].sort == Sort::Asc
                && i.method.is_none()
                && i.predicate.is_none();
            if single {
                let text = if i.name == default {
                    "@unique".to_owned()
                } else {
                    format!("@unique(map: {})", types::string(&i.name))
                };
                if let Some(line) = self.line_of(m, &cols[0])
                    && !line.attributes.iter().any(|a| a.starts_with("@unique"))
                {
                    line.attributes.push(text);
                    continue;
                }
            }
            let args = self.index_args(m, i, &default);
            let kind = if i.unique { "unique" } else { "index" };
            self.models[m].block.attribute(format!("@@{kind}({args})"));
        }
        let keyed = table.primary_key.is_some()
            || table.indexes.iter().any(|i| {
                i.unique
                    && i.predicate.is_none()
                    && !i.expression
                    && i.columns
                        .iter()
                        .all(|c| table.column(&c.name).is_some_and(|x| !x.nullable))
            });
        let ignored = self.models[m]
            .block
            .attributes
            .iter()
            .any(|a| a.text.starts_with("@@ignore"));
        if !keyed && !ignored {
            self.models[m].block.attribute("@@ignore".to_owned());
            self.models[m].block.notes.push(format!(
                "`{model_name}` has no primary key or unique key on required columns, so the client leaves it out (`@@ignore`)"
            ));
        }
        if self.models[m].name != table.name {
            let text = format!("@@map({})", types::string(&table.name));
            self.models[m].block.attribute(text);
        }
        let mut block = std::mem::replace(
            &mut self.models[m].block,
            Block::new("model", String::new()),
        );
        self.schema_attribute(&mut block, &table.schema);
        self.models[m].block = block;
        for c in &self.db.checks {
            if c.schema == table.schema && c.table == table.name {
                self.models[m].block.notes.push(format!(
                    "check constraint `{}`: {} (kept by the database, not by the schema)",
                    c.name, c.definition
                ));
            }
        }
    }

    fn model_of(&self, schema: &str, table: &str) -> Option<usize> {
        self.models
            .iter()
            .position(|m| m.table.schema == schema && m.table.name == table)
    }
}

/// A foreign key that becomes a relation.
struct Fk<'a> {
    holder: usize,
    target: usize,
    fk: &'a ForeignKey,
    one_to_one: bool,
    optional: bool,
}

impl Builder<'_> {
    fn foreign_keys(&mut self) -> Vec<Fk<'_>> {
        let mut out = Vec::new();
        for (h, m) in self.models.iter().enumerate() {
            for fk in &m.table.foreign_keys {
                let Some(t) = self.model_of(&fk.referenced_schema, &fk.referenced_table) else {
                    self.notes.push(format!(
                        "the foreign key `{}` on `{}` points at `{}.{}`, which is not pulled; it was left out",
                        fk.name, m.table.name, fk.referenced_schema, fk.referenced_table
                    ));
                    continue;
                };
                if !is_key(self.models[t].table, &fk.referenced_columns) {
                    self.notes.push(format!(
                        "the foreign key `{}` on `{}` points at columns that are not a key of `{}`; it was left out",
                        fk.name, m.table.name, fk.referenced_table
                    ));
                    continue;
                }
                out.push(Fk {
                    holder: h,
                    target: t,
                    fk,
                    one_to_one: is_key(m.table, &fk.columns),
                    optional: fk
                        .columns
                        .iter()
                        .any(|c| m.table.column(c).is_none_or(|x| x.nullable)),
                });
            }
        }
        out
    }
}

/// Relation fields to add to one model.
struct RelationField {
    model: usize,
    line: Line,
}

fn pair(a: usize, b: usize) -> (usize, usize) {
    (a.min(b), a.max(b))
}

impl Builder<'_> {
    /// NR-4 and NR-5: relation fields on both sides of every foreign key.
    #[allow(
        clippy::too_many_lines,
        reason = "one pass over the foreign keys, kept together"
    )]
    fn relations(&mut self, joins: &[Join]) {
        let fks = self.foreign_keys();
        let fks: Vec<(usize, usize, ForeignKey, bool, bool)> = fks
            .into_iter()
            .map(|f| (f.holder, f.target, f.fk.clone(), f.one_to_one, f.optional))
            .collect();
        let mut count: HashMap<(usize, usize), usize> = HashMap::new();
        for (h, t, ..) in &fks {
            *count.entry(pair(*h, *t)).or_default() += 1;
        }
        for j in joins {
            *count.entry(pair(j.a, j.b)).or_default() += 1;
        }
        let mut added: Vec<RelationField> = Vec::new();
        for (h, t, fk, one_to_one, optional) in &fks {
            let (h, t) = (*h, *t);
            let holder_name = self.models[h].name.clone();
            let target_name = self.models[t].name.clone();
            let needs_name = h == t || count[&pair(h, t)] > 1;
            let old = match (self.models[h].old, self.models[t].old, self.old) {
                (Some(oh), Some(ot), Some(x)) => {
                    x.relation(oh, &fk.columns, ot).map(|r| (x, oh, ot, r))
                }
                _ => None,
            };
            let fields = self.field_list(h, &fk.columns);
            let references = self.field_list(t, &fk.referenced_columns);
            // names: the file's, else NR-4 / NR-5
            let (field, back, relation_name, keep_attribute, old_fields) =
                if let Some((x, oh, ot, r)) = old {
                    let field = x.checked(oh).fields[r.a.field].name.clone();
                    let back = x.checked(ot).fields[r.b.field].name.clone();
                    let named = x.names_relation(oh, r.a.field);
                    let name = if named {
                        Some(r.name.clone())
                    } else if needs_name {
                        Some(format!("{holder_name}{}", upper_first(&field)))
                    } else {
                        None
                    };
                    let same_fk = x
                        .table(oh)
                        .and_then(|tb| tb.foreign_keys.iter().find(|f| f.name == fk.name))
                        == Some(fk);
                    (
                        field,
                        back,
                        name,
                        same_fk && (named || !needs_name),
                        Some((x, oh, ot, r.a.field, r.b.field)),
                    )
                } else {
                    let base = match fk.columns.as_slice() {
                        [one] => {
                            let n = self.field_list(h, std::slice::from_ref(one)).remove(0);
                            match n
                                .strip_suffix("Id")
                                .or_else(|| n.strip_suffix("ID"))
                                .or_else(|| n.strip_suffix("_id"))
                            {
                                Some(stem) if !stem.is_empty() => stem.to_owned(),
                                _ => names::camel(&target_name),
                            }
                        }
                        _ => names::camel(&target_name),
                    };
                    let field = unique(base, &mut self.models[h].taken);
                    let relation_name =
                        needs_name.then(|| format!("{holder_name}{}", upper_first(&field)));
                    let holder_camel = names::camel(&holder_name);
                    let back_base = match (needs_name, *one_to_one) {
                        (false, false) => names::plural(&holder_camel),
                        (false, true) => holder_camel,
                        (true, false) => {
                            format!("{}As{}", names::plural(&holder_camel), upper_first(&field))
                        }
                        (true, true) => format!("{holder_camel}As{}", upper_first(&field)),
                    };
                    let back = unique(back_base, &mut self.models[t].taken);
                    (field, back, relation_name, false, None)
                };
            let name_arg = relation_name.as_ref().map(|n| types::string(n));
            let kept_relation = match &old_fields {
                Some((x, oh, _, af, _)) if keep_attribute => x.block(*oh).fields[*af]
                    .attributes
                    .iter()
                    .find(|a| a.name.name == "relation")
                    .map(|a| x.source(a.span).to_owned()),
                _ => None,
            };
            let relation = if let Some(text) = kept_relation {
                text
            } else {
                let mut args: Vec<String> = name_arg.iter().cloned().collect();
                args.push(format!("fields: {}", list(&fields)));
                args.push(format!("references: {}", list(&references)));
                let required = fk
                    .columns
                    .iter()
                    .any(|c| self.models[h].table.column(c).is_some_and(|x| !x.nullable));
                let default_delete = if required {
                    Action::Restrict
                } else {
                    Action::SetNull
                };
                if fk.on_delete != default_delete {
                    args.push(format!("onDelete: {}", action(fk.on_delete)));
                }
                if fk.on_update != Action::Cascade {
                    args.push(format!("onUpdate: {}", action(fk.on_update)));
                }
                let default_name = cut(
                    &format!("{}_{}", self.models[h].table.name, fk.columns.join("_")),
                    "fkey",
                );
                if fk.name != default_name {
                    args.push(format!("map: {}", types::string(&fk.name)));
                }
                format!("@relation({})", args.join(", "))
            };
            let (holder_around, holder_old, back_around, back_old, kept_holder, kept_back) =
                match &old_fields {
                    Some((x, oh, ot, af, bf)) => {
                        let keep = |model: usize, f: usize| -> Vec<String> {
                            x.block(model).fields[f]
                                .attributes
                                .iter()
                                .filter(|a| a.name.name != "relation")
                                .map(|a| x.source(a.span).to_owned())
                                .collect()
                        };
                        (
                            self.around_field(*x, *oh, *af),
                            Some(*af),
                            self.around_field(*x, *ot, *bf),
                            Some(*bf),
                            keep(*oh, *af),
                            keep(*ot, *bf),
                        )
                    }
                    None => (
                        Around::default(),
                        None,
                        Around::default(),
                        None,
                        Vec::new(),
                        Vec::new(),
                    ),
                };
            let mut holder_attributes = vec![relation];
            holder_attributes.extend(kept_holder);
            added.push(RelationField {
                model: h,
                line: Line {
                    name: field,
                    ty: if *optional {
                        format!("{target_name}?")
                    } else {
                        target_name.clone()
                    },
                    attributes: holder_attributes,
                    around: holder_around,
                    old: holder_old,
                    column: false,
                },
            });
            let mut back_attributes: Vec<String> = name_arg
                .map(|n| format!("@relation({n})"))
                .into_iter()
                .collect();
            back_attributes.extend(kept_back);
            added.push(RelationField {
                model: t,
                line: Line {
                    name: back,
                    ty: if *one_to_one {
                        format!("{holder_name}?")
                    } else {
                        format!("{holder_name}[]")
                    },
                    attributes: back_attributes,
                    around: back_around,
                    old: back_old,
                    column: false,
                },
            });
        }
        for j in joins {
            added.extend(self.many_to_many(j));
        }
        for r in added {
            self.models[r.model].block.lines.push(r.line);
        }
    }

    fn around_field(&self, x: &Existing<'_>, model: usize, field: usize) -> Around {
        let ast = x.block(model);
        let mut spans: Vec<_> = ast.fields.iter().map(|f| f.span).collect();
        spans.extend(ast.attributes.iter().map(|a| a.span));
        x.around(ast.name.span, &spans, ast.span)
            .0
            .swap_remove(field)
    }

    /// NR-6: list fields on both models of a join table.
    fn many_to_many(&mut self, j: &Join) -> Vec<RelationField> {
        let (a_name, b_name) = (self.models[j.a].name.clone(), self.models[j.b].name.clone());
        let default = format!("{a_name}To{b_name}");
        let named = j.name != default || j.a == j.b;
        let name_attribute = named.then(|| format!("@relation({})", types::string(&j.name)));
        let old = self.old.and_then(|x| {
            let r = x.many_to_many(&j.name)?;
            let (oa, ob) = (self.models[j.a].old?, self.models[j.b].old?);
            (r.a.model.0 == oa && r.b.model.0 == ob).then_some((x, r))
        });
        let (fa, fb, around_a, around_b, old_a, old_b) = if let Some((x, r)) = old {
            (
                x.checked(r.a.model.0).fields[r.a.field].name.clone(),
                x.checked(r.b.model.0).fields[r.b.field].name.clone(),
                self.around_field(x, r.a.model.0, r.a.field),
                self.around_field(x, r.b.model.0, r.b.field),
                Some(r.a.field),
                Some(r.b.field),
            )
        } else {
            let fa = unique(
                names::plural(&names::camel(&b_name)),
                &mut self.models[j.a].taken,
            );
            let fb_base = if j.a == j.b {
                format!(
                    "{}As{}",
                    names::plural(&names::camel(&a_name)),
                    names::type_name(&j.name, 'M')
                )
            } else {
                names::plural(&names::camel(&a_name))
            };
            let fb = unique(fb_base, &mut self.models[j.b].taken);
            (fa, fb, Around::default(), Around::default(), None, None)
        };
        let line = |name: String, ty: String, around: Around, old: Option<usize>| Line {
            name,
            ty,
            attributes: name_attribute.iter().cloned().collect(),
            around,
            old,
            column: false,
        };
        vec![
            RelationField {
                model: j.a,
                line: line(fa, format!("{b_name}[]"), around_a, old_a),
            },
            RelationField {
                model: j.b,
                line: line(fb, format!("{a_name}[]"), around_b, old_b),
            },
        ]
    }
}

impl Builder<'_> {
    fn view(&self, v: &View, name: String, old: Option<usize>) -> Block {
        let printed = self.printed_enums();
        let mut block = Block::new("view", name.clone());
        let mut taken = HashSet::new();
        let x = old.and_then(|i| self.old.map(|x| (x, i)));
        let mut around = Vec::new();
        if let Some((x, i)) = x {
            let ast = x.block(i);
            let mut spans: Vec<_> = ast.fields.iter().map(|f| f.span).collect();
            spans.extend(ast.attributes.iter().map(|a| a.span));
            let (a, tail) = x.around(ast.name.span, &spans, ast.span);
            around = a;
            block.above = x.above_top(x.top_of_model(i));
            block.tail = tail;
            block.old = Some(x.top_of_model(i));
            for f in &x.checked(i).fields {
                taken.insert(f.name.clone());
            }
            for (k, attr) in ast.attributes.iter().enumerate() {
                if !matches!(attr.name.name.as_str(), "map" | "schema") {
                    block.attributes.push(BlockAttribute {
                        text: x.source(attr.span).to_owned(),
                        around: around[ast.fields.len() + k].clone(),
                    });
                }
            }
        }
        for (column, ty) in &v.columns {
            let found = x.and_then(|(x, i)| x.field(i, column).map(|f| (x, i, f)));
            let name = match found {
                Some((x, i, f)) => x.checked(i).fields[f].name.clone(),
                None => unique(names::field_name(column), &mut taken),
            };
            let kind = types::column_kind(ty, &printed);
            let mut line = Line {
                name: name.clone(),
                ty: self.type_text(&kind, true),
                attributes: Vec::new(),
                around: Around::default(),
                old: found.map(|(_, _, f)| f),
                column: true,
            };
            if let Kind::Scalar(_, Some(n)) = &kind.kind {
                line.attributes
                    .push(types::native_attribute(&self.datasource, n));
            }
            if let Some((x, i, f)) = found {
                line.around = around[f].clone();
                let ast = &x.block(i).fields[f];
                let base = |t: &str| t.trim_end_matches('?').to_owned();
                if x.column_type(i, f).as_deref() == Some(base(ty).as_str()).or(Some(ty.as_str()))
                    && let Some(t) = &ast.ty
                {
                    line.ty = x.source(t.span).to_owned();
                    line.attributes = ast
                        .attributes
                        .iter()
                        .filter(|a| a.name.name != "map")
                        .map(|a| x.source(a.span).to_owned())
                        .collect();
                }
            }
            if name != *column {
                line.attributes
                    .push(format!("@map({})", types::string(column)));
            }
            block.lines.push(line);
        }
        if name != v.name {
            block.attribute(format!("@@map({})", types::string(&v.name)));
        }
        if self.multi_schema {
            block.attribute(format!("@@schema({})", types::string(&v.schema)));
        }
        block
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
    let joins: Vec<Option<(usize, usize)>> = tables.iter().map(|t| join_shape(t, tables)).collect();
    // the file's names are taken first, so a new block never takes one
    let mut taken: HashSet<String> = HashSet::new();
    let old_model =
        |schema: &str, name: &str, view: bool| old.and_then(|x| x.model(schema, name, view));
    let model_olds: Vec<Option<usize>> = tables
        .iter()
        .map(|t| old_model(&t.schema, &t.name, false))
        .collect();
    let view_olds: Vec<Option<usize>> = db
        .views
        .iter()
        .map(|v| old_model(&v.schema, &v.name, true))
        .collect();
    if let Some(x) = old {
        for o in model_olds.iter().chain(&view_olds).flatten() {
            taken.insert(x.checked(*o).name.clone());
        }
        for e in &db.shape.enums {
            if let Some(i) = x.enumeration(&e.schema, &e.name) {
                taken.insert(x.schema.enums[i].name.clone());
            }
        }
    }
    let mut blocks_enums = b.enums(&mut taken);
    let old_name = |o: Option<usize>| o.and_then(|i| old.map(|x| x.checked(i).name.clone()));
    let mut pending: Vec<usize> = Vec::new();
    for ti in 0..tables.len() {
        if joins[ti].is_some() && model_olds[ti].is_none() {
            pending.push(ti);
        }
    }
    let named = |b: &Builder<'_>, taken: &mut HashSet<String>, ti: usize| {
        b.type_names(
            taken,
            vec![(
                old_name(model_olds[ti]),
                tables[ti].schema.clone(),
                tables[ti].name.clone(),
            )],
        )
        .remove(0)
    };
    let mut model_index: Vec<Option<usize>> = vec![None; tables.len()];
    for (ti, t) in tables.iter().enumerate() {
        if pending.contains(&ti) {
            continue;
        }
        let name = named(&b, &mut taken, ti);
        model_index[ti] = Some(b.models.len());
        b.models.push(PulledModel {
            table: t,
            name: name.clone(),
            old: model_olds[ti],
            fields: Vec::new(),
            taken: HashSet::new(),
            block: Block::new("model", name),
        });
    }
    // a join table is a relation only when its targets are models and `A` is the first by name
    let mut join_list = Vec::new();
    for ti in pending {
        let (a, b_t) = joins[ti].unwrap_or_default();
        if let (Some(ma), Some(mb)) = (model_index[a], model_index[b_t])
            && b.models[ma].name <= b.models[mb].name
        {
            join_list.push(Join {
                name: tables[ti].name[1..].to_owned(),
                a: ma,
                b: mb,
            });
            continue;
        }
        let name = named(&b, &mut taken, ti);
        model_index[ti] = Some(b.models.len());
        b.models.push(PulledModel {
            table: &tables[ti],
            name: name.clone(),
            old: None,
            fields: Vec::new(),
            taken: HashSet::new(),
            block: Block::new("model", name),
        });
    }
    for m in 0..b.models.len() {
        b.columns(m);
        b.keys(m);
    }
    b.relations(&join_list);
    let view_names = b.type_names(
        &mut taken,
        db.views
            .iter()
            .zip(&view_olds)
            .map(|(v, o)| (old_name(*o), v.schema.clone(), v.name.clone()))
            .collect(),
    );
    let mut blocks: Vec<Block> = b
        .models
        .iter_mut()
        .map(|m| std::mem::replace(&mut m.block, Block::new("model", String::new())))
        .collect();
    for (v, (name, o)) in db.views.iter().zip(view_names.into_iter().zip(view_olds)) {
        blocks.push(b.view(v, name, o));
    }
    blocks.append(&mut blocks_enums);
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
    let (mut old, new): (Vec<Line>, Vec<Line>) = lines.into_iter().partition(|l| l.old.is_some());
    old.sort_by_key(|l| l.old);
    let (new_columns, new_relations): (Vec<Line>, Vec<Line>) =
        new.into_iter().partition(|l| l.column);
    let at = old.iter().rposition(|l| l.column).map_or(0, |p| p + 1);
    let at = if old.is_empty() { 0 } else { at };
    let mut out: Vec<Line> = old;
    let tail = out.split_off(at.min(out.len()));
    out.extend(new_columns);
    out.extend(tail);
    out.extend(new_relations);
    block.lines = out;
}
