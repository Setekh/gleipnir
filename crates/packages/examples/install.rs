//! Installs a built-in package's tool into a folder: `cargo run -p gleipnir-packages --example
//! install -- <package> <tool> <managed-dir>`. For setting up test machines without the UI.
use gleipnir_packages::install::{InstallContext, install_tool};
use gleipnir_packages::{detect_sdks, registry};

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let (package_id, tool, dir) = (&args[1], &args[2], std::path::PathBuf::from(&args[3]));
    let package = registry()
        .package(package_id)
        .ok_or_else(|| anyhow::anyhow!("no package {package_id}"))?;
    let spec = package
        .manifest
        .tools
        .get(tool)
        .ok_or_else(|| anyhow::anyhow!("no tool {tool}"))?;
    let base = gleipnir_packages::env::user_env();
    let env = match &package.manifest.sdk {
        Some(sdk_spec) => match detect_sdks(sdk_spec, &base).into_iter().next() {
            Some(sdk) => sdk.env(sdk_spec, &base),
            None => base,
        },
        None => base,
    };
    let path = install_tool(
        tool,
        spec,
        &InstallContext {
            managed_dir: &dir,
            env: &env,
            rust_toolchain: None,
        },
    )?;
    println!("{}", path.display());
    Ok(())
}
