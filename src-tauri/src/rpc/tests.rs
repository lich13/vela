use super::*;
use std::collections::BTreeSet;

/// The names in `export const <array> = names<...>()([...])` of the
/// frontend's contract.
fn contract(array: &str) -> BTreeSet<String> {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../src/bridge-contracts.ts");
    let source = std::fs::read_to_string(path).expect("read the frontend contract");
    let declaration = format!("export const {array} = ");
    let from = source.find(&declaration).expect("the array is declared");
    let open = from + source[from..].find("([").expect("the array opens") + 2;
    let close = open + source[open..].find("])").expect("the array closes");
    let names: Vec<_> = source[open..close]
        .split(',')
        .map(|name| name.trim().trim_matches('"').to_owned())
        .filter(|name| !name.is_empty())
        .collect();
    let unique: BTreeSet<_> = names.iter().cloned().collect();
    assert_eq!(unique.len(), names.len(), "{array} repeats a name");
    unique
}

fn assert_same(what: &str, ours: &BTreeSet<String>, theirs: &BTreeSet<String>) {
    let missing: Vec<_> = theirs.difference(ours).collect();
    let extra: Vec<_> = ours.difference(theirs).collect();
    assert!(
        missing.is_empty() && extra.is_empty(),
        "{what}: the contract has {missing:?} that Rust lacks, and Rust has {extra:?} that the contract lacks"
    );
}

#[test]
fn the_table_holds_exactly_the_shared_commands_of_the_contract() {
    let table: BTreeSet<_> = names().iter().map(|name| name.to_string()).collect();
    assert_eq!(table.len(), names().len(), "the table repeats a name");
    assert_same("shared commands", &table, &contract("sharedCommands"));
}

#[cfg(feature = "desktop")]
#[test]
fn the_desktop_registers_exactly_the_desktop_commands_of_the_contract() {
    // The instance commands arrive with the remote client; until they are
    // registered they are listed here. `remote_call` joins `call` then.
    const PENDING: [&str; 5] = [
        "get_instances",
        "save_instance",
        "delete_instance",
        "select_instance",
        "test_instance",
    ];
    let source = include_str!("../lib.rs");
    let open = "generate_handler![";
    let from = source.find(open).expect("the handler list") + open.len();
    let to = from + source[from..].find(']').expect("the handler list closes");
    let listed: Vec<_> = source[from..to]
        .split(',')
        .map(|path| {
            path.trim()
                .rsplit("::")
                .next()
                .unwrap_or_default()
                .to_owned()
        })
        .filter(|name| !name.is_empty())
        .collect();
    let mut registered: BTreeSet<_> = listed.iter().cloned().collect();
    assert_eq!(registered.len(), listed.len(), "a command is listed twice");
    for name in PENDING {
        assert!(
            registered.insert(name.to_owned()),
            "{name} is registered: drop it from the pending list"
        );
    }
    let mut expected = contract("desktopCommands");
    expected.insert("call".into());
    assert_same("desktop commands", &registered, &expected);
}

#[test]
fn parameter_names_become_camel_case_keys() {
    assert_eq!(camel("id"), "id");
    assert_eq!(camel("client_id"), "clientId");
    assert_eq!(camel("expected_config_revision"), "expectedConfigRevision");
}

#[test]
fn an_absent_argument_is_none_when_optional_and_reported_otherwise() {
    let mut args = Args::new(serde_json::json!({"clientId": "claude", "seconds": "soon"})).unwrap();
    assert_eq!(args.take::<Option<String>>("provider_id").unwrap(), None);
    assert_eq!(args.take::<String>("client_id").unwrap(), "claude");
    let missing = args.take::<String>("expected_revision").unwrap_err();
    assert_eq!(
        (missing.code.as_str(), missing.message.as_str()),
        (code::JSON, "缺少参数 expectedRevision")
    );
    let mistyped = args.take::<u64>("seconds").unwrap_err();
    assert_eq!(mistyped.message, "参数 seconds 无效");
    assert!(Args::new(serde_json::json!([1])).is_err());
    assert!(Args::new(Value::Null).is_ok());
}
