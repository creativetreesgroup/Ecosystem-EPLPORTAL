// Backend/crates/api-gateway/src/auth/permission.rs
//! Centralized permission gate (master spec: "RBAC require_permission
//! terpusat"). The reference scatters ad hoc `if (!session.isMainAccount)`
//! checks across every route file; this enum is the one place that logic
//! lives instead. Every variant is uniformly main-account-gated today (the
//! reference has no finer-grained permission table) — the payoff is that a
//! future finer-grained rule changes ONE function, not N call sites.
use crate::auth::CurrentUser;
use crate::error::ApiError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Permission {
    ManageSubUsers,
    ManageSpxCredentials,
    ManageBotSettings,
    ArmAutoAccept,
    ManagePrices,
    ManageBranding,
    ManageLocations,
    ManageRules,
}

pub fn require_permission(user: &CurrentUser, perm: Permission) -> Result<(), ApiError> {
    let allowed = match perm {
        Permission::ManageSubUsers
        | Permission::ManageSpxCredentials
        | Permission::ManageBotSettings
        | Permission::ArmAutoAccept
        | Permission::ManagePrices
        | Permission::ManageBranding
        | Permission::ManageLocations
        | Permission::ManageRules => user.is_main_account,
    };
    if allowed {
        Ok(())
    } else {
        Err(ApiError::Forbidden)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn user_with(is_main_account: bool) -> CurrentUser {
        CurrentUser {
            session_id: Uuid::new_v4(),
            tenant_id: Uuid::new_v4(),
            portal_user_id: Uuid::new_v4(),
            username: "someone".to_string(),
            display_name: "Someone".to_string(),
            is_main_account,
        }
    }

    /// Explicit array of all 8 variants — no wildcard `_ =>`. If a 9th
    /// variant is ever added and this array isn't updated, that's a future
    /// maintainer's problem to notice via a failing assert count, not
    /// silently skipped.
    const ALL_PERMISSIONS: [Permission; 8] = [
        Permission::ManageSubUsers,
        Permission::ManageSpxCredentials,
        Permission::ManageBotSettings,
        Permission::ArmAutoAccept,
        Permission::ManagePrices,
        Permission::ManageBranding,
        Permission::ManageLocations,
        Permission::ManageRules,
    ];

    #[test]
    fn main_account_is_allowed_every_permission() {
        let user = user_with(true);
        for perm in ALL_PERMISSIONS {
            assert!(
                require_permission(&user, perm).is_ok(),
                "expected {perm:?} to be allowed for main account"
            );
        }
    }

    #[test]
    fn non_main_account_is_forbidden_every_permission() {
        let user = user_with(false);
        for perm in ALL_PERMISSIONS {
            match require_permission(&user, perm) {
                Err(ApiError::Forbidden) => {}
                other => panic!("expected {perm:?} to be Forbidden for non-main account, got {other:?}"),
            }
        }
    }
}
