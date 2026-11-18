//! Fields from columns, values from enum types, and views.

use std::collections::HashSet;

use lumen_schema::ast;

use super::{Block, Builder, Line, PulledEnum};
use crate::postgres::pull::View;
use crate::postgres::pull::existing::{Around, Existing};
use crate::postgres::pull::names::{self, unique};
use crate::postgres::pull::types::{self, ColumnKind, Kind};
use crate::postgres::types::type_name;
use crate::shape::{Column, EnumType};

/// The column type of an enum, as `format_type` may print it: with and without its schema.
fn printed(e: &EnumType) -> Vec<String> {
    let bare = type_name(&e.name);
    vec![format!("{}.{bare}", type_name(&e.schema)), bare]
}

/// The attributes of a file's field the database cannot know (`@updatedAt`, `@ignore`, ..),
/// kept as written.
fn kept_attributes<'a>(
    x: &'a Existing<'_>,
    field: &'a ast::Field,
) -> impl Iterator<Item = String> + 'a {
    field
        .attributes
        .iter()
        .filter(|a| {
            let n = a.name.name.as_str();
            !matches!(n, "id" | "unique" | "default" | "map" | "relation") && !n.contains('.')
        })
        .map(|a| x.source(a.span).to_owned())
}

impl Builder<'_> {
    /// One enum block per enum type; values keep the database's order.
    pub(super) fn enums(&mut self, taken: &mut HashSet<String>) -> Vec<Block> {
        let mut blocks = Vec::new();
        for e in &self.db.shape.enums {
            let old = self
                .old
                .and_then(|x| x.enumeration(&e.schema, &e.name).map(|i| (x, i)));
            let name = self.type_name(
                taken,
                old.map(|(x, i)| x.schema.enums[i].name.clone()),
                &e.schema,
                &e.name,
            );
            let mut block = Block::new("enum", name.clone());
            let mut taken_values = HashSet::new();
            let mut around = Vec::new();
            if let Some((x, i)) = old {
                let (a, tail) = x.enum_around(i);
                let ast = x.enum_block(i);
                block.above = x.above_top(x.top_of_enum(i));
                block.tail = tail;
                block.old = Some(x.top_of_enum(i));
                block.keep_attributes(
                    x,
                    &ast.attributes,
                    &a[ast.values.len()..],
                    &["map", "schema"],
                );
                taken_values.extend(x.schema.enums[i].values.iter().map(|v| v.name.clone()));
                around = a;
            }
            let mut values = Vec::new();
            for db_value in &e.values {
                let found = old.and_then(|(x, i)| {
                    x.schema.enums[i]
                        .values
                        .iter()
                        .position(|v| v.mapped_name.as_deref().unwrap_or(&v.name) == db_value)
                        .map(|k| (x, i, k))
                });
                let mut line = Line::new(String::new(), String::new(), Vec::new(), true);
                if let Some((x, i, k)) = found {
                    line.name.clone_from(&x.schema.enums[i].values[k].name);
                    line.around = around[k].clone();
                    line.old = Some(k);
                    line.attributes.extend(
                        x.enum_block(i).values[k]
                            .attributes
                            .iter()
                            .filter(|a| a.name.name != "map")
                            .map(|a| x.source(a.span).to_owned()),
                    );
                } else {
                    line.name = unique(names::value_name(db_value), &mut taken_values);
                }
                if line.name != *db_value {
                    line.attributes
                        .push(format!("@map({})", types::string(db_value)));
                }
                values.push((db_value.clone(), line.name.clone()));
                block.lines.push(line);
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

    fn column_kind(&self, ty: &str) -> ColumnKind {
        let printed: Vec<Vec<String>> = self.enums.iter().map(|e| e.printed.clone()).collect();
        types::column_kind(ty, &printed)
    }

    /// The field for `column`, keeping the file's type, `@db.X` and `@default` where the database
    /// still agrees with them. `old` is the file's model and field.
    fn column_line(
        &self,
        column: &Column,
        name: String,
        old: Option<(&Existing<'_>, usize, usize)>,
    ) -> (Line, ColumnKind) {
        let kind = self.column_kind(&column.ty);
        let values = match kind.kind {
            Kind::Enum(e) => self.enums[e].values.clone(),
            _ => Vec::new(),
        };
        let mut ty = self.type_text(&kind, column.nullable);
        let mut native = match &kind.kind {
            Kind::Scalar(_, Some(n)) => vec![types::native_attribute(&self.datasource, n)],
            _ => Vec::new(),
        };
        let mut default = column
            .default
            .as_ref()
            .and_then(|d| types::default(&kind, d, &values));
        let mut kept = Vec::new();
        if let Some((x, i, f)) = old {
            let ast = &x.block(i).fields[f];
            let was = x.table(i).and_then(|t| t.column(&column.name));
            if was.is_some_and(|c| c.ty == column.ty && c.nullable == column.nullable)
                && let Some(t) = &ast.ty
            {
                x.source(t.span).clone_into(&mut ty);
                native = ast
                    .attributes
                    .iter()
                    .filter(|a| a.name.name.contains('.'))
                    .map(|a| x.source(a.span).to_owned())
                    .collect();
            }
            if was.is_some_and(|c| c.default == column.default) {
                default = ast
                    .attributes
                    .iter()
                    .find(|a| a.name.name == "default")
                    .map(|a| x.source(a.span).to_owned());
            }
            kept.extend(kept_attributes(x, ast));
        }
        let mut attributes: Vec<String> = default.into_iter().collect();
        if name != column.name {
            attributes.push(format!("@map({})", types::string(&column.name)));
        }
        attributes.extend(native);
        attributes.extend(kept);
        (Line::new(name, ty, attributes, true), kind)
    }

    /// The column fields of model `m`.
    pub(super) fn columns(&mut self, m: usize) {
        let table = self.models[m].table;
        let old = self.old.zip(self.models[m].old);
        let mut around = Vec::new();
        if let Some((x, i)) = old {
            let (a, tail) = x.model_around(i);
            let ast = x.block(i);
            let block = &mut self.models[m].block;
            block.above = x.above_top(x.top_of_model(i));
            block.tail = tail;
            block.old = Some(x.top_of_model(i));
            block.keep_attributes(
                x,
                &ast.attributes,
                &a[ast.fields.len()..],
                &["id", "unique", "index", "map", "schema"],
            );
            // every name the file uses stays taken, relation fields included
            let names = x.checked(i).fields.iter().map(|f| f.name.clone());
            self.models[m].taken.extend(names);
            around = a;
        }
        for column in &table.columns {
            let found = old.and_then(|(x, i)| x.field(i, &column.name).map(|f| (x, i, f)));
            let name = match found {
                Some((x, i, f)) => x.checked(i).fields[f].name.clone(),
                None => unique(names::field_name(&column.name), &mut self.models[m].taken),
            };
            let (mut line, kind) = self.column_line(column, name.clone(), found);
            if let Some((_, _, f)) = found {
                line.around = around[f].clone();
                line.old = Some(f);
            }
            let block = &mut self.models[m].block;
            if let Kind::Unsupported(raw) = &kind.kind {
                block.reports.push(format!(
                    "`{name}` has the type `{raw}`, which the client cannot read (Unsupported)"
                ));
            }
            if kind.list && !column.nullable {
                block.notes.push(format!(
                    "`{name}` is NOT NULL, which a list field cannot say, so `lumen migrate dev` would drop the constraint"
                ));
            }
            self.models[m].fields.push(name);
            self.models[m].block.lines.push(line);
        }
    }

    /// A view: every field optional unless the file says otherwise, no keys.
    pub(super) fn view(&self, v: &View, name: &str, old: Option<usize>) -> Block {
        let mut block = Block::new("view", name.to_owned());
        let mut taken = HashSet::new();
        let old = self.old.zip(old);
        let mut around: Vec<Around> = Vec::new();
        if let Some((x, i)) = old {
            let (a, tail) = x.model_around(i);
            let ast = x.block(i);
            block.above = x.above_top(x.top_of_model(i));
            block.tail = tail;
            block.old = Some(x.top_of_model(i));
            block.keep_attributes(
                x,
                &ast.attributes,
                &a[ast.fields.len()..],
                &["map", "schema"],
            );
            taken.extend(x.checked(i).fields.iter().map(|f| f.name.clone()));
            around = a;
        }
        for (column, ty) in &v.columns {
            let found = old.and_then(|(x, i)| x.field(i, column).map(|f| (x, i, f)));
            let field = match found {
                Some((x, i, f)) => x.checked(i).fields[f].name.clone(),
                None => unique(names::field_name(column), &mut taken),
            };
            let kind = self.column_kind(ty);
            let mut line = Line::new(field.clone(), self.type_text(&kind, true), Vec::new(), true);
            if let Kind::Scalar(_, Some(n)) = &kind.kind {
                line.attributes
                    .push(types::native_attribute(&self.datasource, n));
            }
            if let Some((x, i, f)) = found {
                line.around = around[f].clone();
                line.old = Some(f);
                let ast = &x.block(i).fields[f];
                // the database says nothing about a view column's arity or keys: keep the file's
                if x.column_type(i, f).as_deref() == Some(ty.as_str())
                    && let Some(t) = &ast.ty
                {
                    x.source(t.span).clone_into(&mut line.ty);
                    line.attributes = ast
                        .attributes
                        .iter()
                        .filter(|a| a.name.name != "map")
                        .map(|a| x.source(a.span).to_owned())
                        .collect();
                }
            }
            if field != *column {
                line.attributes
                    .push(format!("@map({})", types::string(column)));
            }
            block.lines.push(line);
        }
        if name != v.name {
            block.attribute(format!("@@map({})", types::string(&v.name)));
        }
        self.schema_attribute(&mut block, &v.schema);
        block
    }
}
