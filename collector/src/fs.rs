//! One way to write a file this process owns: JSON, mode 0600, in a 0700
//! directory, by rename — the grant (`oidc.rs`) and the calendar cursors
//! (`calendars.rs`) are written the same way, so a crash between the
//! temporary and the rename leaves the previous file whole.

use std::path::Path;

use anyhow::{Context, Result};

pub fn write_json_private(path: &Path, value: &impl serde::Serialize) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let directory = path
        .parent()
        .with_context(|| format!("{} has no parent directory", path.display()))?;
    std::fs::create_dir_all(directory)
        .with_context(|| format!("failed to create {}", directory.display()))?;
    std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))
        .with_context(|| format!("failed to set the mode of {}", directory.display()))?;
    let temporary = directory.join(format!(
        ".{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("file.json")
    ));
    {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&temporary)
            .with_context(|| format!("failed to open {}", temporary.display()))?;
        file.write_all(serde_json::to_string_pretty(value)?.as_bytes())?;
        file.write_all(b"\n")?;
        file.sync_all()?;
    }
    std::fs::rename(&temporary, path)
        .with_context(|| format!("failed to move {} into place", temporary.display()))?;
    Ok(())
}
