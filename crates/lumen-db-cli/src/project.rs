//! Finding the schema, reading `.env`, and the database URL, the way Prisma does.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use lumen_schema::checked::{Schema, StringOrEnv};
use lumen_schema::{SourceFile, check};

/// A command failed; the message is printed as it is.
pub type Fail = String;

/// The schema file: `--schema`, else `./schema.prisma`, else `./prisma/schema.prisma`.
///
/// # Errors
/// When none of them exists.
pub fn schema_path(given: Option<&Path>) -> Result<PathBuf, Fail> {
    if let Some(p) = given {
        return if p.exists() {
            Ok(p.to_path_buf())
        } else {
            Err(format!("there is no schema at {}", p.display()))
        };
    }
    for candidate in ["schema.prisma", "prisma/schema.prisma"] {
        let p = PathBuf::from(candidate);
        if p.exists() {
            return Ok(p);
        }
    }
    Err("no schema.prisma here or in ./prisma; run `lumen init` or pass --schema".to_owned())
}

/// Variables from `.env` files (next to the schema, then the current folder). The real
/// environment wins over them; the first file that sets a variable wins over later ones.
#[derive(Debug, Default, Clone)]
pub struct Env {
    vars: HashMap<String, String>,
}

impl Env {
    /// Read the `.env` files for `schema`.
    #[must_use]
    pub fn load(schema: &Path) -> Self {
        let mut files = Vec::new();
        if let Some(dir) = schema.parent().filter(|d| !d.as_os_str().is_empty()) {
            files.push(dir.join(".env"));
        }
        files.push(PathBuf::from(".env"));
        let mut vars = HashMap::new();
        for file in files {
            let Ok(text) = std::fs::read_to_string(&file) else {
                continue;
            };
            for line in text.lines() {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                let line = line.strip_prefix("export ").unwrap_or(line);
                let Some((key, value)) = line.split_once('=') else {
                    continue;
                };
                let value = value.trim();
                let value = value
                    .strip_prefix('"')
                    .and_then(|v| v.strip_suffix('"'))
                    .or_else(|| value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))
                    .unwrap_or(value);
                vars.entry(key.trim().to_owned())
                    .or_insert_with(|| value.to_owned());
            }
        }
        Self { vars }
    }

    /// The variable `name`: from the environment, else from `.env`.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<String> {
        std::env::var(name)
            .ok()
            .or_else(|| self.vars.get(name).cloned())
    }
}

/// The schema text and its checked form; problems are printed with the source line.
///
/// # Errors
/// When the file cannot be read or has errors.
pub fn load(path: &Path) -> Result<(SourceFile, Schema), Fail> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let file = SourceFile::new(path.display().to_string(), text);
    let (schema, diagnostics) = check(&file);
    if diagnostics.has_errors() {
        return Err(diagnostics.render(&file).trim_end().to_owned());
    }
    if !diagnostics.is_empty() {
        eprint!("{}", diagnostics.render(&file));
    }
    Ok((file, schema))
}

/// The datasource URL, from the file or the environment variable it names.
///
/// # Errors
/// When there is no datasource, no URL, or the variable is not set.
pub fn database_url(schema: &Schema, env: &Env) -> Result<String, Fail> {
    let ds = schema
        .datasource
        .as_ref()
        .ok_or("the schema has no datasource block")?;
    match &ds.url {
        Some(StringOrEnv::Literal(url)) => Ok(url.clone()),
        Some(StringOrEnv::Env(var)) => env.get(var).ok_or_else(|| {
            format!("the environment variable {var} is not set; put it in .env or the shell")
        }),
        None => Err("the datasource has no url".to_owned()),
    }
}

/// The shadow database URL, when the datasource sets one.
#[must_use]
pub fn shadow_url(schema: &Schema, env: &Env) -> Option<String> {
    match schema.datasource.as_ref()?.shadow_database_url.as_ref()? {
        StringOrEnv::Literal(url) => Some(url.clone()),
        StringOrEnv::Env(var) => env.get(var),
    }
}

/// The database name in a MongoDB URL (`mongodb://host/app?..` → `app`).
///
/// # Errors
/// When the URL names no database.
pub fn mongo_database(url: &str) -> Result<String, Fail> {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let path = rest.split_once('/').map(|(_, p)| p).unwrap_or_default();
    let name = path.split('?').next().unwrap_or_default();
    if name.is_empty() {
        Err(
            "the MongoDB url names no database; add it, for example mongodb://localhost:27017/app"
                .to_owned(),
        )
    } else {
        Ok(name.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::mongo_database;

    #[test]
    fn mongo_database_names() {
        assert_eq!(
            mongo_database("mongodb://u:p@h:27017/app?replicaSet=rs0").as_deref(),
            Ok("app")
        );
        assert_eq!(
            mongo_database("mongodb+srv://cluster.example.net/shop").as_deref(),
            Ok("shop")
        );
        assert!(mongo_database("mongodb://localhost:27017").is_err());
    }
}
