//! Language support as data: what each language is, which tools it needs, where they come from, and
//! the toolchain (SDK) a project builds with.
//!
//! A **package** describes a language family in a manifest. A **tool** is an executable a package
//! needs, like gopls. An **SDK** is the toolchain a project uses, like a Go installation. The five
//! built-in languages are packages like any other, embedded in the binary.

pub mod adapters;
pub mod dirs;
pub mod env;
pub mod install;
pub mod manifest;
pub mod registry;
pub mod sdk;
pub mod settings;
pub mod testing;
pub mod tools;

pub use adapters::{CommandSpec, DebugLaunch, RunTarget, RunTargetKind, adapter_for};
pub use dirs::{config_dir, managed_tools_dir, state_dir};
pub use env::Env;
pub use manifest::{InstallSpec, Manifest, ToolSpec};
pub use registry::{LanguageEntry, Package, Registry, registry};
pub use sdk::{Sdk, detect_sdks};
pub use testing::{
    JS_NAME_SEPARATOR, JS_REPORTER_MARK, Suite, TestFormat, TestPick, is_js_test_file, js_report_of,
};
pub use tools::{Resolution, ResolveContext, ToolSource, resolve_tool};
