//! Read a live Postgres database into a [`Shape`]. One query per kind of object, against
//! `pg_catalog`, so column, key and index order are exactly the database's.

use sqlx::{PgPool, Row};

use super::types::normalise_default;
use crate::shape::{
    Action, Column, Default, EnumType, ForeignKey, Index, IndexColumn, PrimaryKey, Shape, Sort,
    Table,
};

/// The table that records applied migrations; never part of the shape.
pub const MIGRATIONS_TABLE: &str = "_prisma_migrations";

const TABLES: &str = "
select n.nspname as schema, c.relname as name
from pg_class c join pg_namespace n on n.oid = c.relnamespace
where c.relkind in ('r', 'p') and not c.relispartition
  and n.nspname = any($1) and c.relname <> $2
order by 1, 2";

const COLUMNS: &str = "
select n.nspname as schema, c.relname as table, a.attname as name,
       format_type(a.atttypid, a.atttypmod) as ty, not a.attnotnull as nullable,
       pg_get_expr(d.adbin, d.adrelid) as default_expr, a.attidentity::text as identity
from pg_attribute a
join pg_class c on c.oid = a.attrelid
join pg_namespace n on n.oid = c.relnamespace
left join pg_attrdef d on d.adrelid = a.attrelid and d.adnum = a.attnum
where c.relkind in ('r', 'p') and a.attnum > 0 and not a.attisdropped
  and n.nspname = any($1)
order by 1, 2, a.attnum";

const CONSTRAINTS: &str = "
select n.nspname as schema, c.relname as table, con.conname as name, con.contype::text as kind,
       array(select a.attname from unnest(con.conkey) with ordinality k(num, ord)
             join pg_attribute a on a.attrelid = con.conrelid and a.attnum = k.num
             order by k.ord)::text[] as columns,
       rn.nspname as ref_schema, rc.relname as ref_table,
       array(select a.attname from unnest(con.confkey) with ordinality k(num, ord)
             join pg_attribute a on a.attrelid = con.confrelid and a.attnum = k.num
             order by k.ord)::text[] as ref_columns,
       con.confdeltype::text as on_delete, con.confupdtype::text as on_update
from pg_constraint con
join pg_class c on c.oid = con.conrelid
join pg_namespace n on n.oid = c.relnamespace
left join pg_class rc on rc.oid = con.confrelid
left join pg_namespace rn on rn.oid = rc.relnamespace
where con.contype in ('p', 'f') and n.nspname = any($1)
order by 1, 2, 3";

const INDEXES: &str = "
select n.nspname as schema, t.relname as table, i.relname as name, x.indisunique as is_unique,
       am.amname as method,
       array(select case when k.num = 0 then pg_get_indexdef(x.indexrelid, k.ord::int, true)
                         else a.attname::text end
             from unnest(x.indkey) with ordinality k(num, ord)
             left join pg_attribute a on a.attrelid = x.indrelid and a.attnum = k.num
             where k.ord <= x.indnkeyatts
             order by k.ord)::text[] as columns,
       exists(select 1 from unnest(x.indkey) k(num) where k.num = 0) as expression,
       pg_get_expr(x.indpred, x.indrelid, true) as predicate,
       array(select (o.opt & 1) = 1 from unnest(x.indoption) with ordinality o(opt, ord)
             order by o.ord)::bool[] as descending
from pg_index x
join pg_class i on i.oid = x.indexrelid
join pg_class t on t.oid = x.indrelid
join pg_namespace n on n.oid = t.relnamespace
join pg_am am on am.oid = i.relam
where not x.indisprimary and n.nspname = any($1)
  and not exists (select 1 from pg_constraint c where c.conindid = x.indexrelid and c.contype = 'x')
order by 1, 2, 3";

const ENUMS: &str = "
select n.nspname as schema, t.typname as name,
       array(select e.enumlabel::text from pg_enum e where e.enumtypid = t.oid
             order by e.enumsortorder)::text[] as values
from pg_type t join pg_namespace n on n.oid = t.typnamespace
where t.typtype = 'e' and n.nspname = any($1)
order by 1, 2";

fn action(code: &str) -> Action {
    match code {
        "c" => Action::Cascade,
        "r" => Action::Restrict,
        "n" => Action::SetNull,
        "d" => Action::SetDefault,
        _ => Action::NoAction,
    }
}

/// Numeric columns keep numeric defaults unquoted: Postgres prints `DEFAULT 0` on a `bigint`
/// as `'0'::bigint`.
fn numeric_default(ty: &str, default: Default) -> Default {
    let numeric = [
        "smallint",
        "integer",
        "bigint",
        "real",
        "double precision",
        "numeric",
        "money",
    ]
    .iter()
    .any(|t| ty.starts_with(t));
    match default {
        Default::Literal(l) if numeric => {
            let inner = l.trim_matches('\'');
            if inner.parse::<f64>().is_ok() {
                Default::Literal(inner.to_owned())
            } else {
                Default::Literal(l)
            }
        }
        other => other,
    }
}

fn find(shape: &Shape, schema: &str, table: &str) -> Option<usize> {
    shape
        .tables
        .iter()
        .position(|t| t.schema == schema && t.name == table)
}

async fn constraints(
    pool: &PgPool,
    schemas: &[String],
    shape: &mut Shape,
) -> Result<(), sqlx::Error> {
    for row in sqlx::query(CONSTRAINTS)
        .bind(schemas)
        .fetch_all(pool)
        .await?
    {
        let (schema, table): (String, String) = (row.try_get("schema")?, row.try_get("table")?);
        let Some(t) = find(shape, &schema, &table) else {
            continue;
        };
        let name: String = row.try_get("name")?;
        let columns: Vec<String> = row.try_get("columns")?;
        let kind: String = row.try_get("kind")?;
        if kind == "p" {
            shape.tables[t].primary_key = Some(PrimaryKey { name, columns });
        } else {
            let on_delete: String = row.try_get("on_delete")?;
            let on_update: String = row.try_get("on_update")?;
            shape.tables[t].foreign_keys.push(ForeignKey {
                name,
                columns,
                referenced_schema: row.try_get("ref_schema")?,
                referenced_table: row.try_get("ref_table")?,
                referenced_columns: row.try_get("ref_columns")?,
                on_delete: action(&on_delete),
                on_update: action(&on_update),
            });
        }
    }
    Ok(())
}

async fn indexes(pool: &PgPool, schemas: &[String], shape: &mut Shape) -> Result<(), sqlx::Error> {
    for row in sqlx::query(INDEXES).bind(schemas).fetch_all(pool).await? {
        let (schema, table): (String, String) = (row.try_get("schema")?, row.try_get("table")?);
        let Some(t) = find(shape, &schema, &table) else {
            continue;
        };
        let names: Vec<String> = row.try_get("columns")?;
        let descending: Vec<bool> = row.try_get("descending")?;
        let method: String = row.try_get("method")?;
        shape.tables[t].indexes.push(Index {
            name: row.try_get("name")?,
            columns: names
                .into_iter()
                .zip(descending.into_iter().chain(std::iter::repeat(false)))
                .map(|(name, desc)| IndexColumn {
                    name,
                    sort: if desc { Sort::Desc } else { Sort::Asc },
                })
                .collect(),
            unique: row.try_get("is_unique")?,
            method: (method != "btree").then_some(method),
            predicate: row.try_get("predicate")?,
            expression: row.try_get("expression")?,
        });
    }
    Ok(())
}

/// The shape of the database behind `pool`, for the given schemas (namespaces).
///
/// # Errors
/// When a catalog query fails.
pub async fn describe(pool: &PgPool, schemas: &[String]) -> Result<Shape, sqlx::Error> {
    let mut shape = Shape::default();
    for row in sqlx::query(TABLES)
        .bind(schemas)
        .bind(MIGRATIONS_TABLE)
        .fetch_all(pool)
        .await?
    {
        shape.tables.push(Table {
            schema: row.try_get("schema")?,
            name: row.try_get("name")?,
            columns: Vec::new(),
            primary_key: None,
            indexes: Vec::new(),
            foreign_keys: Vec::new(),
        });
    }
    for row in sqlx::query(COLUMNS).bind(schemas).fetch_all(pool).await? {
        let (schema, table): (String, String) = (row.try_get("schema")?, row.try_get("table")?);
        let Some(t) = find(&shape, &schema, &table) else {
            continue;
        };
        let ty: String = row.try_get("ty")?;
        let identity: Option<String> = row.try_get("identity")?;
        let default = if identity.as_deref().is_some_and(|i| !i.is_empty()) {
            Some(Default::Sequence)
        } else {
            row.try_get::<Option<String>, _>("default_expr")?
                .map(|e| numeric_default(&ty, normalise_default(&e)))
        };
        shape.tables[t].columns.push(Column {
            name: row.try_get("name")?,
            ty,
            nullable: row.try_get("nullable")?,
            default,
        });
    }
    constraints(pool, schemas, &mut shape).await?;
    indexes(pool, schemas, &mut shape).await?;
    for row in sqlx::query(ENUMS).bind(schemas).fetch_all(pool).await? {
        shape.enums.push(EnumType {
            schema: row.try_get("schema")?,
            name: row.try_get("name")?,
            values: row.try_get("values")?,
        });
    }
    shape.normalise();
    Ok(shape)
}
