//! Allocated SQLite pages, including ciphertext, indices, retained history and free pages.
//! This is an active-database bound, not a filesystem/WAL/backup or commercial allowance.
use super::*;
use serde::Serialize;

#[derive(Debug, Clone, Copy, Serialize)]
pub struct DatabaseAllocation {
    pub page_size: u64,
    pub allocated_pages: u64,
    pub reusable_pages: u64,
    pub allocated_bytes: u64,
}
impl IntakeStore {
    pub fn database_allocation(&self, scope: &AssetScope) -> Result<DatabaseAllocation> {
        self.check(scope)?;
        let page_size: u64 = self
            .connection
            .query_row("PRAGMA page_size", [], |r| r.get(0))?;
        let allocated_pages: u64 = self
            .connection
            .query_row("PRAGMA page_count", [], |r| r.get(0))?;
        let reusable_pages: u64 = self
            .connection
            .query_row("PRAGMA freelist_count", [], |r| r.get(0))?;
        let allocated_bytes = page_size
            .checked_mul(allocated_pages)
            .ok_or_else(|| invalid("database_allocation_overflow"))?;
        if !(512..=65536).contains(&page_size) || reusable_pages > allocated_pages {
            return Err(invalid("invalid_database_allocation"));
        }
        Ok(DatabaseAllocation {
            page_size,
            allocated_pages,
            reusable_pages,
            allocated_bytes,
        })
    }
    /// Per-connection enforcement. The hosting adapter must reapply its durable policy
    /// on every opened connection while holding its shared asset lock.
    pub fn limit_database_allocation(&self, scope: &AssetScope, maximum_bytes: u64) -> Result<()> {
        let current = self.database_allocation(scope)?;
        if maximum_bytes == 0
            || maximum_bytes > (1_u64 << 40)
            || !maximum_bytes.is_multiple_of(current.page_size)
            || maximum_bytes < current.allocated_bytes
        {
            return Err(invalid("database_capacity_invalid_or_below_allocation"));
        }
        let pages = maximum_bytes / current.page_size;
        let actual: u64 =
            self.connection
                .query_row(&format!("PRAGMA max_page_count={pages}"), [], |r| r.get(0))?;
        if actual != pages {
            return Err(invalid("database_capacity_not_applied"));
        }
        // Keep finished WALs small; these settings are not a physical filesystem quota.
        self.connection
            .execute_batch("PRAGMA wal_autocheckpoint=1; PRAGMA journal_size_limit=0;")?;
        Ok(())
    }
}
