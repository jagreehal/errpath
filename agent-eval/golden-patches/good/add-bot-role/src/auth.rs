#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Admin,
    Member,
    Guest,
    Bot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Read,
    Write,
    ManageBots,
}

/// Returns whether `role` may perform `action`.
pub fn authorize(role: Role, action: Action) -> bool {
    match role {
        Role::Admin => true,
        Role::Member => matches!(action, Action::Read | Action::Write),
        Role::Guest => matches!(action, Action::Read),
        Role::Bot => matches!(action, Action::Read | Action::ManageBots),
    }
}
