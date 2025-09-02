// Migrations are constructed by domain, and stored in a table in the connection db with domain name,
// effected tables, actual query text, and order.
// If a migration is run and any of the query texts don't match, the app panics on startup (maybe fallback
// to creating a new db?)
// Otherwise any missing migrations are run on the connection

use std::ffi::CString;

use anyhow::{Context as _, Result};
use indoc::{formatdoc, indoc};
use libsqlite3_sys::sqlite3_exec;

use crate::connection::Connection;

impl Connection {
    fn eager_exec(&self, sql: &str) -> anyhow::Result<()> {
        let sql_str = CString::new(sql).context("Error creating cstr")?;
        unsafe {
            sqlite3_exec(
                self.sqlite3,
                sql_str.as_c_str().as_ptr(),
                None,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            );
        }
        self.last_error()
            .with_context(|| format!("Prepare call failed for query:\n{}", sql))?;

        Ok(())
    }

    /// Migrate the database, for the given domain.
    /// Note: Unlike everything else in SQLez, migrations are run eagerly, without first
    /// preparing the SQL statements. This makes it possible to do multi-statement schema
    /// updates in a single string without running into prepare errors.
    pub fn migrate(
        &self,
        domain: &'static str,
        migrations: &[&'static str],
        mut should_allow_migration_change: impl FnMut(usize, &str, &str) -> bool,
    ) -> Result<()> {
        self.with_savepoint("migrating", || {
            // Setup the migrations table unconditionally
            self.exec(indoc! {"
                CREATE TABLE IF NOT EXISTS migrations (
                    domain TEXT,
                    step INTEGER,
                    migration TEXT
                )"})?()?;

            let completed_migrations =
                self.select_bound::<&str, (String, usize, String)>(indoc! {"
                    SELECT domain, step, migration FROM migrations
                    WHERE domain = ?
                    ORDER BY step
                    "})?(domain)?;

            let mut store_completed_migration = self
                .exec_bound("INSERT INTO migrations (domain, step, migration) VALUES (?, ?, ?)")?;

            let mut did_migrate = false;
            for (index, migration) in migrations.iter().enumerate() {
                let migration =
                    sqlformat::format(migration, &sqlformat::QueryParams::None, Default::default());
                if let Some((_, _, completed_migration)) = completed_migrations.get(index) {
                    // Reformat completed migrations with the current `sqlformat` version, so that past migrations stored
                    // conform to the new formatting rules.
                    let completed_migration = sqlformat::format(
                        completed_migration,
                        &sqlformat::QueryParams::None,
                        Default::default(),
                    );
                    if completed_migration == migration {
                        // Migration already run. Continue
                        continue;
                    } else if should_allow_migration_change(index, &completed_migration, &migration)
                    {
                        continue;
                    } else {
                        anyhow::bail!(formatdoc! {"
                            Migration changed for {domain} at step {index}

                            Stored migration:
                            {completed_migration}

                            Proposed migration:
                            {migration}"});
                    }
                }

                self.eager_exec(&migration)?;
                did_migrate = true;
                store_completed_migration((domain, index, migration))?;
            }

            if did_migrate {
                self.delete_rows_with_orphaned_foreign_key_references()?;
                self.exec("PRAGMA foreign_key_check;")?()?;
            }

            Ok(())
        })
    }

    /// Delete any rows that were orphaned by a migration. This is needed
    /// because we disable foreign key constraints during migrations, so
    /// that it's possible to re-create a table with the same name, without
    /// deleting all associated data.
    fn delete_rows_with_orphaned_foreign_key_references(&self) -> Result<()> {
        let foreign_key_info: Vec<(String, String, String, String)> = self.select(
            r#"
                SELECT DISTINCT
                    schema.name as child_table,
                    foreign_keys.[from] as child_key,
                    foreign_keys.[table] as parent_table,
                    foreign_keys.[to] as parent_key
                FROM sqlite_schema schema
                JOIN pragma_foreign_key_list(schema.name) foreign_keys
                WHERE
                    schema.type = 'table' AND
                    schema.name NOT LIKE "sqlite_%"
            "#,
        )?()?;

        if !foreign_key_info.is_empty() {
            log::info!(
                "Found {} foreign key relationships to check",
                foreign_key_info.len()
            );
        }

        for (child_table, child_key, parent_table, parent_key) in foreign_key_info {
            self.exec(&format!(
                "
                DELETE FROM {child_table}
                WHERE {child_key} IS NOT NULL and {child_key} NOT IN
                (SELECT {parent_key} FROM {parent_table})
                "
            ))?()?;
        }

        Ok(())
    }
}
