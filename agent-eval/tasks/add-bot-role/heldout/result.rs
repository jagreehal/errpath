use checkout::{Action, Role, authorize};

#[test]
fn bot_can_read_and_manage_bots() {
    assert!(authorize(Role::Bot, Action::Read));
    assert!(authorize(Role::Bot, Action::ManageBots));
}

#[test]
fn bot_cannot_write() {
    assert!(!authorize(Role::Bot, Action::Write));
}

#[test]
fn existing_roles_unchanged() {
    assert!(authorize(Role::Admin, Action::ManageBots));
    assert!(authorize(Role::Member, Action::Write));
    assert!(!authorize(Role::Member, Action::ManageBots));
    assert!(authorize(Role::Guest, Action::Read));
    assert!(!authorize(Role::Guest, Action::Write));
}
