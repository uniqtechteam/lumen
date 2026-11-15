//! The database shape a checked schema asks for, with Prisma's rules so lumen and Prisma create
//! the same tables: Prisma's default Postgres types, constraint names (`{table}_pkey`,
//! `{table}_{cols}_key`, `_idx`, `_fkey`, cut to 63 bytes), default referential actions, and
//! implicit many-to-many tables `_{Relation}` with columns `A` and `B`.

use lumen_schema::ast::Arity;
use lumen_schema::checked::{
    DefaultValue, FieldKind, IndexKind, Literal, Model, ModelId, ModelKind, ReferentialAction,
    RelationKind, Schema, SortOrder,
};

use crate::postgres::types::{column_type, string_literal, type_name};
use crate::shape::{
    Action, Column, Default, EnumType, ForeignKey, Index, IndexColumn, PrimaryKey, Shape, Sort,
    Table,
};

/// Postgres cuts identifiers at 63 bytes; Prisma's names are cut the same way.
const MAX_IDENTIFIER: usize = 63;

fn cut(name: String) -> String {
    if name.len() <= MAX_IDENTIFIER {
        return name;
    }
    let mut end = MAX_IDENTIFIER;
    while !name.is_char_boundary(end) {
        end -= 1;
    }
    name[..end].to_owned()
}

fn schema_of(model: &Model) -> String {
    model.schema.clone().unwrap_or_else(|| "public".to_owned())
}

fn action(a: ReferentialAction) -> Action {
    match a {
        ReferentialAction::Cascade => Action::Cascade,
        ReferentialAction::Restrict => Action::Restrict,
        ReferentialAction::NoAction => Action::NoAction,
        ReferentialAction::SetNull => Action::SetNull,
        ReferentialAction::SetDefault => Action::SetDefault,
    }
}

fn literal_sql(l: &Literal) -> String {
    match l {
        Literal::String(s) | Literal::EnumValue(s) => string_literal(s),
        Literal::Number(n) => n.clone(),
        Literal::Boolean(b) => b.to_string(),
        Literal::List(items) => {
            let inner: Vec<String> = items.iter().map(literal_sql).collect();
            format!("ARRAY[{}]", inner.join(", "))
        }
    }
}

fn default_of(d: &DefaultValue) -> Option<Default> {
    match d {
        DefaultValue::Autoincrement => Some(Default::Sequence),
        DefaultValue::Now => Some(Default::Now),
        DefaultValue::Literal(l) => Some(Default::Literal(literal_sql(l))),
        DefaultValue::DbGenerated(Some(sql)) => Some(Default::Expression(sql.clone())),
        // made by the client, not the database
        DefaultValue::Uuid(_)
        | DefaultValue::Cuid(_)
        | DefaultValue::Nanoid(_)
        | DefaultValue::Ulid
        | DefaultValue::Auto
        | DefaultValue::DbGenerated(None) => None,
    }
}

fn columns(schema: &Schema, model: &Model) -> Vec<Column> {
    model
        .fields
        .iter()
        .filter_map(|f| {
            let base = match &f.kind {
                FieldKind::Scalar(s) => column_type(*s, f.native_type.as_ref()),
                FieldKind::Enum(id) => {
                    let e = &schema.enums[id.0];
                    type_name(e.mapped_name.as_deref().unwrap_or(&e.name))
                }
                FieldKind::Unsupported(raw) => raw.clone(),
                FieldKind::Model(_) | FieldKind::Composite(_) | FieldKind::Unknown => return None,
            };
            let ty = if f.arity == Arity::List {
                format!("{base}[]")
            } else {
                base
            };
            Some(Column {
                name: f.database_name().to_owned(),
                ty,
                nullable: f.arity == Arity::Optional,
                default: f.default.as_ref().and_then(default_of),
            })
        })
        .collect()
}

fn column_names(model: &Model, fields: &[usize]) -> Vec<String> {
    fields
        .iter()
        .map(|&i| model.fields[i].database_name().to_owned())
        .collect()
}

fn indexes(model: &Model, table: &str) -> (Option<PrimaryKey>, Vec<Index>) {
    let mut primary = None;
    let mut out = Vec::new();
    for index in &model.indexes {
        let cols: Vec<usize> = index.fields.iter().map(|f| f.field).collect();
        let names = column_names(model, &cols);
        let joined = names.join("_");
        match index.kind {
            IndexKind::PrimaryKey => {
                primary = Some(PrimaryKey {
                    name: index
                        .mapped_name
                        .clone()
                        .unwrap_or_else(|| cut(format!("{table}_pkey"))),
                    columns: names,
                });
            }
            IndexKind::Unique | IndexKind::Normal | IndexKind::Fulltext => {
                let unique = index.kind == IndexKind::Unique;
                let suffix = if unique { "key" } else { "idx" };
                out.push(Index {
                    name: index
                        .mapped_name
                        .clone()
                        .unwrap_or_else(|| cut(format!("{table}_{joined}_{suffix}"))),
                    columns: index
                        .fields
                        .iter()
                        .map(|f| IndexColumn {
                            name: model.fields[f.field].database_name().to_owned(),
                            sort: if f.sort == Some(SortOrder::Desc) {
                                Sort::Desc
                            } else {
                                Sort::Asc
                            },
                        })
                        .collect(),
                    unique,
                    method: index
                        .algorithm
                        .as_ref()
                        .map(|a| a.to_lowercase())
                        .filter(|a| a != "btree"),
                });
            }
        }
    }
    (primary, out)
}

/// The id columns of a model, for join tables.
fn single_id(model: &Model) -> Option<(String, String)> {
    let pk = model.primary_key()?;
    let [field] = pk.fields.as_slice() else {
        return None;
    };
    let f = &model.fields[field.field];
    let FieldKind::Scalar(s) = f.kind else {
        return None;
    };
    Some((
        f.database_name().to_owned(),
        column_type(s, f.native_type.as_ref()),
    ))
}

fn join_table(schema: &Schema, name: &str, a: ModelId, b: ModelId) -> Option<Table> {
    let (ma, mb) = (&schema.models[a.0], &schema.models[b.0]);
    let ((a_col, a_ty), (b_col, b_ty)) = (single_id(ma)?, single_id(mb)?);
    let table = format!("_{name}");
    let fk = |column: &str, target: &Model, target_col: String| ForeignKey {
        name: cut(format!("{table}_{column}_fkey")),
        columns: vec![column.to_owned()],
        referenced_schema: schema_of(target),
        referenced_table: target.database_name().to_owned(),
        referenced_columns: vec![target_col],
        on_delete: Action::Cascade,
        on_update: Action::Cascade,
    };
    Some(Table {
        schema: schema_of(ma),
        name: table.clone(),
        columns: vec![
            Column {
                name: "A".into(),
                ty: a_ty,
                nullable: false,
                default: None,
            },
            Column {
                name: "B".into(),
                ty: b_ty,
                nullable: false,
                default: None,
            },
        ],
        primary_key: Some(PrimaryKey {
            name: cut(format!("{table}_AB_pkey")),
            columns: vec!["A".into(), "B".into()],
        }),
        indexes: vec![Index {
            name: cut(format!("{table}_B_index")),
            columns: vec![IndexColumn {
                name: "B".into(),
                sort: Sort::Asc,
            }],
            unique: false,
            method: None,
        }],
        foreign_keys: vec![fk("A", ma, a_col), fk("B", mb, b_col)],
    })
}

/// The shape `schema` asks for (Postgres).
#[must_use]
pub fn calculate(schema: &Schema) -> Shape {
    let mut shape = Shape::default();
    for e in &schema.enums {
        shape.enums.push(EnumType {
            schema: e.schema.clone().unwrap_or_else(|| "public".to_owned()),
            name: e.mapped_name.clone().unwrap_or_else(|| e.name.clone()),
            values: e
                .values
                .iter()
                .map(|v| v.mapped_name.clone().unwrap_or_else(|| v.name.clone()))
                .collect(),
        });
    }
    for model in &schema.models {
        if model.kind == ModelKind::View || model.ignored {
            continue;
        }
        let table = model.database_name().to_owned();
        let (primary_key, indexes) = indexes(model, &table);
        shape.tables.push(Table {
            schema: schema_of(model),
            name: table,
            columns: columns(schema, model),
            primary_key,
            indexes,
            foreign_keys: Vec::new(),
        });
    }
    for relation in &schema.relations {
        let (a, b) = (relation.a.model, relation.b.model);
        if relation.kind == RelationKind::ManyToMany {
            // A is the first model by name; the relation records it as `a`
            if let Some(t) = join_table(schema, &relation.name, a, b) {
                shape.tables.push(t);
            }
            continue;
        }
        let (ma, mb) = (&schema.models[a.0], &schema.models[b.0]);
        if ma.kind == ModelKind::View || mb.kind == ModelKind::View || ma.ignored || mb.ignored {
            continue;
        }
        let cols = column_names(ma, &relation.fields);
        let required = relation
            .fields
            .iter()
            .any(|&f| ma.fields[f].arity == Arity::Required);
        let foreign_key = ForeignKey {
            name: relation
                .mapped_name
                .clone()
                .unwrap_or_else(|| cut(format!("{}_{}_fkey", ma.database_name(), cols.join("_")))),
            columns: cols,
            referenced_schema: schema_of(mb),
            referenced_table: mb.database_name().to_owned(),
            referenced_columns: column_names(mb, &relation.references),
            // Prisma's defaults: Restrict for a required relation, SetNull for an optional one
            on_delete: relation.on_delete.map_or(
                if required {
                    Action::Restrict
                } else {
                    Action::SetNull
                },
                action,
            ),
            on_update: relation.on_update.map_or(Action::Cascade, action),
        };
        if let Some(t) = shape
            .tables
            .iter_mut()
            .find(|t| t.schema == schema_of(ma) && t.name == ma.database_name())
        {
            t.foreign_keys.push(foreign_key);
        }
    }
    shape.normalise();
    shape
}
