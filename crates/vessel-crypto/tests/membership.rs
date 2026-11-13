use std::collections::HashMap;

use vessel_crypto::Id;
use vessel_crypto::identity::{UserKeys, UserPublicKeys};
use vessel_crypto::membership::{Action, Membership, Role, SignedEvent, replay};

const VAULT: Id = [7; 16];

struct World {
    keys: HashMap<Id, UserKeys>,
    events: Vec<SignedEvent>,
    state: Membership,
}

impl World {
    fn new(owner: Id) -> Self {
        let mut w = Self {
            keys: HashMap::new(),
            events: Vec::new(),
            state: Membership::default(),
        };
        for id in [owner, [2; 16], [3; 16], [4; 16]] {
            w.keys.insert(id, UserKeys::generate().unwrap());
        }
        w.push(Action::Create, owner, Some(Role::Owner), owner)
            .unwrap();
        w
    }

    fn public(&self) -> impl Fn(&Id) -> Option<UserPublicKeys> + '_ {
        |id| self.keys.get(id).map(UserKeys::public)
    }

    fn push(
        &mut self,
        action: Action,
        subject: Id,
        role: Option<Role>,
        actor: Id,
    ) -> Result<(), vessel_crypto::CryptoError> {
        let event = self
            .state
            .next_event(VAULT, action, subject, role, actor)
            .sign(&self.keys[&actor]);
        let keys = self.keys[&actor].public();
        let mut next = self.state.clone();
        next.apply(&VAULT, &event, &keys)?;
        self.state = next;
        self.events.push(event);
        Ok(())
    }
}

const OWNER: Id = [1; 16];
const ALICE: Id = [2; 16];
const BOB: Id = [3; 16];
const EVE: Id = [4; 16];

#[test]
fn replay_matches_incremental_state() {
    let mut w = World::new(OWNER);
    w.push(Action::Add, ALICE, Some(Role::Admin), OWNER)
        .unwrap();
    w.push(Action::Add, BOB, Some(Role::Editor), ALICE).unwrap();
    w.push(Action::ChangeRole, BOB, Some(Role::Viewer), ALICE)
        .unwrap();
    w.push(Action::Remove, BOB, None, BOB).unwrap();

    let state = replay(&VAULT, &w.events, w.public()).unwrap();
    assert_eq!(state, w.state);
    assert_eq!(state.role_of(&OWNER), Some(Role::Owner));
    assert_eq!(state.role_of(&ALICE), Some(Role::Admin));
    assert_eq!(state.role_of(&BOB), None);
}

#[test]
fn non_managers_cannot_change_membership() {
    let mut w = World::new(OWNER);
    w.push(Action::Add, ALICE, Some(Role::Editor), OWNER)
        .unwrap();
    assert!(w.push(Action::Add, BOB, Some(Role::Viewer), ALICE).is_err());
    assert!(
        w.push(Action::Add, EVE, Some(Role::Viewer), EVE).is_err(),
        "outsider added themself"
    );
}

#[test]
fn admins_cannot_touch_owner_or_peers() {
    let mut w = World::new(OWNER);
    w.push(Action::Add, ALICE, Some(Role::Admin), OWNER)
        .unwrap();
    w.push(Action::Add, BOB, Some(Role::Admin), OWNER).unwrap();
    assert!(w.push(Action::Remove, OWNER, None, ALICE).is_err());
    assert!(w.push(Action::Remove, BOB, None, ALICE).is_err());
    assert!(
        w.push(Action::ChangeRole, BOB, Some(Role::Viewer), ALICE)
            .is_err()
    );
    assert!(w.push(Action::Add, EVE, Some(Role::Owner), ALICE).is_err());
    w.push(Action::Remove, BOB, None, OWNER).unwrap();
}

#[test]
fn owner_cannot_leave() {
    let mut w = World::new(OWNER);
    assert!(w.push(Action::Remove, OWNER, None, OWNER).is_err());
}

#[test]
fn forged_or_reordered_history_is_rejected() {
    let mut w = World::new(OWNER);
    w.push(Action::Add, ALICE, Some(Role::Editor), OWNER)
        .unwrap();
    w.push(Action::Add, BOB, Some(Role::Editor), OWNER).unwrap();

    // Server drops an event from the middle.
    let mut dropped = w.events.clone();
    dropped.remove(1);
    assert!(replay(&VAULT, &dropped, w.public()).is_err());

    // Server injects Eve, signing with Eve's key while claiming the owner did it.
    let mut forged = w
        .state
        .next_event(VAULT, Action::Add, EVE, Some(Role::Admin), OWNER)
        .sign(&w.keys[&EVE]);
    let mut injected = w.events.clone();
    injected.push(forged.clone());
    assert!(replay(&VAULT, &injected, w.public()).is_err());

    // Server tampers with a role after signing.
    forged = w
        .state
        .next_event(VAULT, Action::Add, EVE, Some(Role::Viewer), OWNER)
        .sign(&w.keys[&OWNER]);
    forged.body.role = Some(Role::Admin);
    injected = w.events.clone();
    injected.push(forged);
    assert!(replay(&VAULT, &injected, w.public()).is_err());

    // History of another vault.
    assert!(replay(&[9; 16], &w.events, w.public()).is_err());
    assert!(replay(&VAULT, &[], w.public()).is_err());
}
