use anyhow::{anyhow, Result};
use rusqlite::{params, OptionalExtension};

use super::Store;
use crate::model::PermissionPolicy;

const PERMISSION_POLICY_KEY: &str = "permission_policy";

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

impl Store {
    pub fn get_setting(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT value FROM settings WHERE key = ?1",
                params![key],
                |row| row.get(0),
            )
            .optional()?)
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO settings (key, value, updated)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(key) DO UPDATE SET
                value = excluded.value,
                updated = excluded.updated",
            params![key, value, now_secs()],
        )?;
        Ok(())
    }

    pub fn remove_setting(&self, key: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM settings WHERE key = ?1", params![key])?;
        Ok(())
    }

    pub fn get_permission_policy(&self) -> Result<Option<PermissionPolicy>> {
        let Some(value) = self.get_setting(PERMISSION_POLICY_KEY)? else {
            return Ok(None);
        };
        value
            .parse()
            .map(Some)
            .map_err(|_| anyhow!("invalid permission_policy setting value '{value}'"))
    }

    pub fn set_permission_policy(&self, policy: PermissionPolicy) -> Result<()> {
        self.set_setting(PERMISSION_POLICY_KEY, policy.as_str())
    }

    pub fn reset_permission_policy(&self) -> Result<()> {
        self.remove_setting(PERMISSION_POLICY_KEY)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permission_policy_round_trips_and_resets() {
        let store = Store::open_in_memory().unwrap();
        assert_eq!(store.get_permission_policy().unwrap(), None);

        store
            .set_permission_policy(PermissionPolicy::AlwaysYolo)
            .unwrap();
        assert_eq!(
            store.get_permission_policy().unwrap(),
            Some(PermissionPolicy::AlwaysYolo)
        );

        store
            .set_permission_policy(PermissionPolicy::InheritKnown)
            .unwrap();
        assert_eq!(
            store.get_permission_policy().unwrap(),
            Some(PermissionPolicy::InheritKnown)
        );

        store.reset_permission_policy().unwrap();
        assert_eq!(store.get_permission_policy().unwrap(), None);
    }
}
