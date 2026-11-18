//! Relations from foreign keys (NR-4, NR-5) and implicit many-to-many join tables (NR-6).

use std::collections::HashMap;

use super::{Builder, Join, Line, is_key, list, upper_first};
use crate::calculate::cut;
use crate::postgres::pull::existing::{Around, Existing};
use crate::postgres::pull::names::{self, unique};
use crate::postgres::pull::types;
use crate::shape::{Action, ForeignKey};

fn action(a: Action) -> &'static str {
    match a {
        Action::Cascade => "Cascade",
        Action::Restrict => "Restrict",
        Action::NoAction => "NoAction",
        Action::SetNull => "SetNull",
        Action::SetDefault => "SetDefault",
    }
}

fn pair(a: usize, b: usize) -> (usize, usize) {
    (a.min(b), a.max(b))
}

/// A foreign key that becomes a relation.
struct Fk {
    holder: usize,
    target: usize,
    key: ForeignKey,
    one_to_one: bool,
    optional: bool,
}

/// The file's relation for a foreign key: its two fields.
#[derive(Clone, Copy)]
struct OldRelation<'x> {
    x: &'x Existing<'x>,
    holder: (usize, usize),
    back: (usize, usize),
    named: bool,
    name: &'x str,
}

impl OldRelation<'_> {
    fn field(&self, (model, field): (usize, usize)) -> String {
        self.x.checked(model).fields[field].name.clone()
    }

    fn around(&self, (model, field): (usize, usize)) -> Around {
        self.x.model_around(model).0.swap_remove(field)
    }

    /// The field's attributes other than `@relation`, as written.
    fn kept(&self, (model, field): (usize, usize)) -> Vec<String> {
        self.x.block(model).fields[field]
            .attributes
            .iter()
            .filter(|a| a.name.name != "relation")
            .map(|a| self.x.source(a.span).to_owned())
            .collect()
    }

    fn relation_attribute(&self) -> Option<String> {
        let (model, field) = self.holder;
        self.x.block(model).fields[field]
            .attributes
            .iter()
            .find(|a| a.name.name == "relation")
            .map(|a| self.x.source(a.span).to_owned())
    }
}

/// The names of one relation: the field on the holder, the field on the target, the relation.
struct Names {
    field: String,
    back: String,
    relation: Option<String>,
}

impl<'a> Builder<'a> {
    fn foreign_keys(&mut self) -> Vec<Fk> {
        let mut out = Vec::new();
        for h in 0..self.models.len() {
            let table = self.models[h].table;
            for key in &table.foreign_keys {
                let Some(t) = self.model_of(&key.referenced_schema, &key.referenced_table) else {
                    self.notes.push(format!(
                        "the foreign key `{}` on `{}` points at `{}.{}`, which is not pulled; it was left out",
                        key.name, table.name, key.referenced_schema, key.referenced_table
                    ));
                    continue;
                };
                if !is_key(self.models[t].table, &key.referenced_columns) {
                    self.notes.push(format!(
                        "the foreign key `{}` on `{}` points at columns that are not a key of `{}`; it was left out",
                        key.name, table.name, key.referenced_table
                    ));
                    continue;
                }
                out.push(Fk {
                    holder: h,
                    target: t,
                    key: key.clone(),
                    one_to_one: is_key(table, &key.columns),
                    optional: key
                        .columns
                        .iter()
                        .any(|c| table.column(c).is_none_or(|x| x.nullable)),
                });
            }
        }
        out
    }

    fn old_relation(&self, fk: &Fk) -> Option<OldRelation<'a>> {
        let x = self.old?;
        let (h, t) = (self.models[fk.holder].old?, self.models[fk.target].old?);
        let r = x.relation(h, &fk.key.columns, t)?;
        Some(OldRelation {
            x,
            holder: (h, r.a.field),
            back: (t, r.b.field),
            named: x.names_relation(h, r.a.field),
            name: &r.name,
        })
    }

    /// NR-4, NR-5: the file's names where it has the relation, else new ones.
    fn names(&mut self, fk: &Fk, old: Option<OldRelation<'_>>, needs_name: bool) -> Names {
        let holder = self.models[fk.holder].name.clone();
        if let Some(o) = old {
            let field = o.field(o.holder);
            let relation = if o.named {
                Some(o.name.to_owned())
            } else {
                needs_name.then(|| format!("{holder}{}", upper_first(&field)))
            };
            return Names {
                field,
                back: o.field(o.back),
                relation,
            };
        }
        let target = &self.models[fk.target].name;
        let base = match fk.key.columns.as_slice() {
            [one] => {
                let n = self
                    .field_list(fk.holder, std::slice::from_ref(one))
                    .remove(0);
                match n.strip_suffix("Id").or_else(|| n.strip_suffix("ID")) {
                    Some(stem) if !stem.is_empty() => stem.to_owned(),
                    _ => names::camel(target),
                }
            }
            _ => names::camel(target),
        };
        let field = unique(base, &mut self.models[fk.holder].taken);
        let holder_camel = names::camel(&holder);
        let back = match (needs_name, fk.one_to_one) {
            (false, false) => names::plural(&holder_camel),
            (false, true) => holder_camel,
            (true, false) => format!("{}As{}", names::plural(&holder_camel), upper_first(&field)),
            (true, true) => format!("{holder_camel}As{}", upper_first(&field)),
        };
        Names {
            relation: needs_name.then(|| format!("{holder}{}", upper_first(&field))),
            back: unique(back, &mut self.models[fk.target].taken),
            field,
        }
    }

    fn relation_attribute(&self, fk: &Fk, relation: Option<&String>) -> String {
        let table = self.models[fk.holder].table;
        let mut args: Vec<String> = relation.map(|n| types::string(n)).into_iter().collect();
        args.push(format!(
            "fields: {}",
            list(&self.field_list(fk.holder, &fk.key.columns))
        ));
        args.push(format!(
            "references: {}",
            list(&self.field_list(fk.target, &fk.key.referenced_columns))
        ));
        // the defaults: Restrict when a column is required, else SetNull; Cascade on update
        let required = fk
            .key
            .columns
            .iter()
            .any(|c| table.column(c).is_some_and(|x| !x.nullable));
        let default_delete = if required {
            Action::Restrict
        } else {
            Action::SetNull
        };
        if fk.key.on_delete != default_delete {
            args.push(format!("onDelete: {}", action(fk.key.on_delete)));
        }
        if fk.key.on_update != Action::Cascade {
            args.push(format!("onUpdate: {}", action(fk.key.on_update)));
        }
        let default_name = cut(
            &format!("{}_{}", table.name, fk.key.columns.join("_")),
            "fkey",
        );
        if fk.key.name != default_name {
            args.push(format!("map: {}", types::string(&fk.key.name)));
        }
        format!("@relation({})", args.join(", "))
    }

    /// The two relation fields of one foreign key.
    fn relation_lines(&mut self, fk: &Fk, needs_name: bool) -> [(usize, Line); 2] {
        let old = self.old_relation(fk);
        let names = self.names(fk, old, needs_name);
        let (holder, target) = (
            self.models[fk.holder].name.clone(),
            self.models[fk.target].name.clone(),
        );
        // the file's `@relation(..)` stays when the foreign key is as it says and the name is settled
        let same_key = old.is_some_and(|o| {
            o.x.table(o.holder.0)
                .and_then(|t| t.foreign_keys.iter().find(|f| f.name == fk.key.name))
                == Some(&fk.key)
        });
        let kept = old
            .filter(|o| same_key && (o.named || !needs_name))
            .and_then(|o| o.relation_attribute());
        let relation = kept.unwrap_or_else(|| self.relation_attribute(fk, names.relation.as_ref()));
        let ty = if fk.optional {
            format!("{target}?")
        } else {
            target
        };
        let mut field = Line::new(names.field, ty, vec![relation], false);
        let back_ty = if fk.one_to_one {
            format!("{holder}?")
        } else {
            format!("{holder}[]")
        };
        let back_attribute = names
            .relation
            .as_ref()
            .map(|n| format!("@relation({})", types::string(n)));
        let mut back = Line::new(
            names.back,
            back_ty,
            back_attribute.into_iter().collect(),
            false,
        );
        if let Some(o) = old {
            field.attributes.extend(o.kept(o.holder));
            field.around = o.around(o.holder);
            field.old = Some(o.holder.1);
            back.attributes.extend(o.kept(o.back));
            back.around = o.around(o.back);
            back.old = Some(o.back.1);
        }
        [(fk.holder, field), (fk.target, back)]
    }

    /// Relation fields on both sides of every foreign key and join table.
    pub(super) fn relations(&mut self, joins: &[Join]) {
        let fks = self.foreign_keys();
        let mut count: HashMap<(usize, usize), usize> = HashMap::new();
        for fk in &fks {
            *count.entry(pair(fk.holder, fk.target)).or_default() += 1;
        }
        for j in joins {
            *count.entry(pair(j.a, j.b)).or_default() += 1;
        }
        let mut lines = Vec::new();
        for fk in &fks {
            let needs_name = fk.holder == fk.target || count[&pair(fk.holder, fk.target)] > 1;
            lines.extend(self.relation_lines(fk, needs_name));
        }
        for j in joins {
            lines.extend(self.many_to_many(j));
        }
        for (m, line) in lines {
            self.models[m].block.lines.push(line);
        }
    }

    /// NR-6: list fields on both models of a join table.
    fn many_to_many(&mut self, j: &Join) -> [(usize, Line); 2] {
        let (a_name, b_name) = (self.models[j.a].name.clone(), self.models[j.b].name.clone());
        let named = j.name != format!("{a_name}To{b_name}") || j.a == j.b;
        let attributes: Vec<String> = named
            .then(|| format!("@relation({})", types::string(&j.name)))
            .into_iter()
            .collect();
        let mut a = Line::new(
            String::new(),
            format!("{b_name}[]"),
            attributes.clone(),
            false,
        );
        let mut b = Line::new(String::new(), format!("{a_name}[]"), attributes, false);
        let old = self.old.and_then(|x| {
            let r = x.many_to_many(&j.name)?;
            let ok = Some(r.a.model.0) == self.models[j.a].old
                && Some(r.b.model.0) == self.models[j.b].old;
            ok.then_some((x, r))
        });
        if let Some((x, r)) = old {
            for (line, side) in [(&mut a, r.a), (&mut b, r.b)] {
                line.name
                    .clone_from(&x.checked(side.model.0).fields[side.field].name);
                line.around = x.model_around(side.model.0).0.swap_remove(side.field);
                line.old = Some(side.field);
            }
        } else {
            a.name = unique(
                names::plural(&names::camel(&b_name)),
                &mut self.models[j.a].taken,
            );
            let b_base = if j.a == j.b {
                format!(
                    "{}As{}",
                    names::plural(&names::camel(&a_name)),
                    names::type_name(&j.name, 'M')
                )
            } else {
                names::plural(&names::camel(&a_name))
            };
            b.name = unique(b_base, &mut self.models[j.b].taken);
        }
        [(j.a, a), (j.b, b)]
    }
}
