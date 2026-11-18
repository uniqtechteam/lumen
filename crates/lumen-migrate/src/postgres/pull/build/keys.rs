//! Primary keys, unique keys and indexes as `@id`, `@unique`, `@@id`, `@@unique`, `@@index`, and
//! `@@ignore` for a table the client cannot address.

use std::collections::HashSet;

use lumen_schema::checked::IndexKind;

use super::{BlockAttribute, Builder, Line, list};
use crate::calculate::cut;
use crate::postgres::pull::types;
use crate::shape::{Index, Sort};

fn index_type(method: &str) -> &str {
    match method {
        "hash" => "Hash",
        "gin" => "Gin",
        "gist" => "Gist",
        "brin" => "Brin",
        "spgist" => "SpGist",
        other => other,
    }
}

impl Builder<'_> {
    /// The field names of `columns` of model `m`.
    pub(super) fn field_list(&self, m: usize, columns: &[String]) -> Vec<String> {
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

    /// The file's key and index attributes that still describe the database, kept as written
    /// where they are; the names of the database keys they cover.
    fn kept_keys(&mut self, m: usize) -> HashSet<String> {
        let mut covered = HashSet::new();
        let Some((x, i)) = self.old.zip(self.models[m].old) else {
            return covered;
        };
        let table = self.models[m].table;
        let Some(want) = x.table(i) else {
            return covered;
        };
        let ast = x.block(i);
        let (around, _) = x.model_around(i);
        for index in &x.checked(i).indexes {
            let name = if index.kind == IndexKind::PrimaryKey {
                let name = index
                    .mapped_name
                    .clone()
                    .unwrap_or_else(|| cut(&table.name, "pkey"));
                if table.primary_key.is_none() || table.primary_key != want.primary_key {
                    continue;
                }
                name
            } else {
                let cols: Vec<&str> = index
                    .fields
                    .iter()
                    .map(|f| x.checked(i).fields[f.field].database_name())
                    .collect();
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
                if db.is_none() || db != want.indexes.iter().find(|j| j.name == name) {
                    continue;
                }
                name
            };
            covered.insert(name);
            // the attribute that made it: on a field, or in the block
            let text = x.source(index.span).to_owned();
            if let Some(f) = ast
                .fields
                .iter()
                .position(|f| f.attributes.iter().any(|a| a.span == index.span))
            {
                let line = self.models[m]
                    .block
                    .lines
                    .iter_mut()
                    .find(|l| l.column && l.old == Some(f));
                if let Some(line) = line {
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

    fn index_args(&self, m: usize, i: &Index, default_name: &str) -> String {
        let columns: Vec<String> = i.columns.iter().map(|c| c.name.clone()).collect();
        let fields: Vec<String> = self
            .field_list(m, &columns)
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
            args.push(format!("type: {}", index_type(method)));
        }
        if let Some(p) = &i.predicate {
            args.push(format!("where: raw({})", types::string(p)));
        }
        args.join(", ")
    }

    fn primary_key(&mut self, m: usize, covered: &HashSet<String>) {
        let table = self.models[m].table;
        let Some(pk) = table
            .primary_key
            .as_ref()
            .filter(|pk| !covered.contains(&pk.name))
        else {
            return;
        };
        let map = (pk.name != cut(&table.name, "pkey"))
            .then(|| format!("map: {}", types::string(&pk.name)));
        if let [column] = pk.columns.as_slice() {
            let text = map.map_or_else(|| "@id".to_owned(), |map| format!("@id({map})"));
            if let Some(line) = self.line_of(m, column) {
                line.attributes.insert(0, text);
            }
        } else {
            let mut args = vec![list(&self.field_list(m, &pk.columns))];
            args.extend(map);
            self.models[m]
                .block
                .attribute(format!("@@id({})", args.join(", ")));
        }
    }

    fn index(&mut self, m: usize, i: &Index) {
        let table = self.models[m].table;
        if i.expression {
            let cols: Vec<&str> = i.columns.iter().map(|c| c.name.as_str()).collect();
            self.models[m].block.notes.push(format!(
                "the index `{}` is on an expression ({}); the schema cannot say it, so `lumen migrate dev` would drop it",
                i.name,
                cols.join(", ")
            ));
            return;
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
                return;
            }
        }
        let args = self.index_args(m, i, &default);
        let kind = if i.unique { "unique" } else { "index" };
        self.models[m].block.attribute(format!("@@{kind}({args})"));
    }

    /// Keys, indexes, `@@ignore`, `@@map`, `@@schema` and the notes for model `m`.
    pub(super) fn keys(&mut self, m: usize) {
        let covered = self.kept_keys(m);
        self.primary_key(m, &covered);
        let table = self.models[m].table;
        for i in &table.indexes {
            if !covered.contains(&i.name) {
                self.index(m, i);
            }
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
        let block = &mut self.models[m].block;
        if !keyed
            && !block
                .attributes
                .iter()
                .any(|a| a.text.starts_with("@@ignore"))
        {
            block.attribute("@@ignore".to_owned());
            let note = format!(
                "`{}` has no primary key or unique key on required columns, so the client leaves it out (`@@ignore`)",
                block.name
            );
            block.notes.push(note);
        }
        if block.name != table.name {
            block.attribute(format!("@@map({})", types::string(&table.name)));
        }
        if self.multi_schema {
            block.attribute(format!("@@schema({})", types::string(&table.schema)));
        }
        for c in &self.db.checks {
            if c.schema == table.schema && c.table == table.name {
                block.notes.push(format!(
                    "the check constraint `{}` ({}) is kept by the database; the schema cannot say it",
                    c.name, c.definition
                ));
            }
        }
    }
}
