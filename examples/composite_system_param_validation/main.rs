//! Composite `SystemParam` preparation validation matrix.
//!
//! Run with `cargo run --example composite_system_param_validation`.

use grass_scheduler::prelude::*;
use std::any::{Any, TypeId};
use std::cell::RefCell;
use std::collections::HashMap;
use std::panic::{catch_unwind, AssertUnwindSafe};

#[derive(Clone, Copy, Debug)]
enum Update {
    Run,
}

impl ScheduleSet for Update {
    fn to_index(&self) -> u32 {
        0
    }

    fn name(&self) -> &'static str {
        "Update"
    }
}

/// A simulation-agnostic resource whose nested state is checked before execution.
struct Contract {
    enabled: bool,
}

/// A composite parameter that delegates retrieval to `Res<Contract>` and adds its
/// own startup contract validation.
struct CheckedContract<'a>(Res<'a, Contract>);

impl SystemParam for CheckedContract<'_> {
    type Item<'new> = CheckedContract<'new>;

    fn retrieve<'r>(
        resources: &'r [RefCell<Box<dyn Any>>],
        index: usize,
        locals: *mut HashMap<TypeId, Box<dyn Any>>,
    ) -> Self::Item<'r> {
        CheckedContract(Res::<Contract>::retrieve(resources, index, locals))
    }

    fn resource_type_id() -> Option<(TypeId, &'static str)> {
        Some((TypeId::of::<Contract>(), std::any::type_name::<Contract>()))
    }

    fn access_kind() -> AccessKind {
        AccessKind::Read
    }

    fn validate(resources: &[RefCell<Box<dyn Any>>], index: usize) -> Vec<String> {
        let resource = resources[index].borrow();
        let contract = resource
            .downcast_ref::<Contract>()
            .expect("scheduler resolved Contract at its concrete resource slot");
        (!contract.enabled)
            .then(|| "contract.enabled must be true".to_string())
            .into_iter()
            .collect()
    }
}

fn consume(contract: CheckedContract<'_>) {
    assert!(
        contract.0.enabled,
        "validated systems only run enabled contracts"
    );
}

fn expected(case: &str) -> String {
    let config: toml::Value = include_str!("config.toml")
        .parse()
        .expect("example config is valid TOML");
    config["checks"][case]
        .as_str()
        .expect("each case has a declarative expected outcome")
        .to_string()
}

fn valid_nested_contract() -> bool {
    let mut scheduler = Scheduler::default();
    scheduler.add_resource(Contract { enabled: true });
    scheduler.add_update_system(consume, Update::Run);
    scheduler.organize_systems();
    scheduler.run();
    true
}

fn panic_message(payload: Box<dyn Any + Send>) -> String {
    match payload.downcast::<String>() {
        Ok(message) => *message,
        Err(payload) => payload
            .downcast::<&'static str>()
            .map(|message| (*message).to_string())
            .unwrap_or_else(|_| "non-string panic payload".to_string()),
    }
}

fn invalid_nested_contract() -> bool {
    let panic = catch_unwind(AssertUnwindSafe(|| {
        let mut scheduler = Scheduler::default();
        scheduler.add_resource(Contract { enabled: false });
        scheduler.add_update_system(consume, Update::Run);
        scheduler.organize_systems();
    }))
    .expect_err("an invalid nested contract must fail during scheduler preparation");
    let message = panic_message(panic);
    message.contains("Schedule validation errors")
        && message.contains("contract.enabled must be true")
        && message.contains("consume")
}

fn main() {
    type ValidationCase = (&'static str, fn() -> bool);
    let cases: [ValidationCase; 2] = [
        ("valid_nested_contract", valid_nested_contract),
        ("invalid_nested_contract", invalid_nested_contract),
    ];
    let mut passed = 0;
    for (case, check) in cases {
        let wanted = expected(case);
        let actual = if check() { "PASS" } else { "FAIL" };
        let matches = actual == wanted;
        println!("{case}: expected={wanted} actual={actual} pass={matches}");
        passed += usize::from(matches);
    }
    assert_eq!(
        passed, 2,
        "every composite validation case must match its expectation"
    );
    println!("composite_system_param_validation: passed={passed}/2");
}
