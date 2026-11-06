//! The variables tree for the selected frame: watches first, then each scope, children fetched
//! as they're opened. What's open is remembered by name path, so it stays open from stop to stop
//! even though the adapter's references change.

use gleipnir_core::{Session, WatchValue};
use std::collections::HashSet;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Row {
    Watch {
        index: usize,
        expression: String,
        value: WatchShown,
        key: String,
        expanded: bool,
    },
    Scope {
        name: String,
        key: String,
        expanded: bool,
    },
    Variable {
        name: String,
        value: String,
        type_name: Option<String>,
        reference: i64,
        depth: usize,
        key: String,
        expanded: bool,
    },
    /// Children on their way.
    Loading { depth: usize },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WatchShown {
    Pending,
    Value { text: String, reference: i64 },
    Error(String),
}

impl Row {
    pub fn key(&self) -> Option<&str> {
        match self {
            Self::Watch { key, .. } | Self::Scope { key, .. } | Self::Variable { key, .. } => {
                Some(key)
            }
            Self::Loading { .. } => None,
        }
    }

    /// The adapter's reference for a row's children, if it has any.
    pub fn reference(&self) -> Option<i64> {
        match self {
            Self::Watch {
                value: WatchShown::Value { reference, .. },
                ..
            }
            | Self::Variable { reference, .. } => (*reference > 0).then_some(*reference),
            _ => None,
        }
    }
}

/// The rows to show, and the references whose children are open but not fetched yet.
pub fn rows(
    session: Option<&Session>,
    watches: &[String],
    toggled: &HashSet<String>,
) -> (Vec<Row>, Vec<i64>) {
    let mut rows = Vec::new();
    let mut missing = Vec::new();
    for (index, expression) in watches.iter().enumerate() {
        let value = match session.and_then(|s| s.watches.get(index)) {
            None | Some(WatchValue::Pending) => WatchShown::Pending,
            Some(WatchValue::Value(result)) => WatchShown::Value {
                text: result.result.clone(),
                reference: result.variables_reference,
            },
            Some(WatchValue::Error(error)) => WatchShown::Error(error.clone()),
        };
        let key = format!("watch:{expression}");
        let expanded = toggled.contains(&key);
        let row = Row::Watch {
            index,
            expression: expression.clone(),
            value,
            key: key.clone(),
            expanded,
        };
        let reference = row.reference();
        rows.push(row);
        if let (true, Some(reference), Some(session)) = (expanded, reference, session) {
            children(
                session,
                reference,
                1,
                &key,
                toggled,
                &mut rows,
                &mut missing,
            );
        }
    }
    let Some(session) = session else {
        return (rows, missing);
    };
    for scope in &session.scopes {
        let key = format!("scope:{}", scope.name);
        // The frame's own variables start open; globals and registers don't.
        let open_by_default = gleipnir_core::is_local_scope(scope);
        let expanded = open_by_default != toggled.contains(&key);
        rows.push(Row::Scope {
            name: scope.name.clone(),
            key: key.clone(),
            expanded,
        });
        if expanded {
            children(
                session,
                scope.variables_reference,
                1,
                &key,
                toggled,
                &mut rows,
                &mut missing,
            );
        }
    }
    (rows, missing)
}

fn children(
    session: &Session,
    reference: i64,
    depth: usize,
    parent: &str,
    toggled: &HashSet<String>,
    rows: &mut Vec<Row>,
    missing: &mut Vec<i64>,
) {
    let Some(variables) = session.variables.get(&reference) else {
        missing.push(reference);
        rows.push(Row::Loading { depth });
        return;
    };
    for variable in variables {
        let key = format!("{parent}/{}", variable.name);
        let expanded = variable.variables_reference > 0 && toggled.contains(&key);
        rows.push(Row::Variable {
            name: variable.name.clone(),
            value: variable.value.clone(),
            type_name: variable.type_name.clone(),
            reference: variable.variables_reference,
            depth,
            key: key.clone(),
            expanded,
        });
        // Deep or cyclic structures stop somewhere.
        if expanded && depth < 12 {
            children(
                session,
                variable.variables_reference,
                depth + 1,
                &key,
                toggled,
                rows,
                missing,
            );
        }
    }
}
