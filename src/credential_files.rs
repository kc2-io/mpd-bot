//! App-owned credential files. Contents are excluded from settings and diagnostics.
//! File access is restricted before writing secret bytes; no OS credential vault is used.
use std::{
    fs,
    io::{Read, Write},
    path::Path,
};
const MAX_BYTES: u64 = 64 * 1024;

fn reject_link(path: &Path) -> Result<(), ()> {
    let metadata = fs::symlink_metadata(path).map_err(|_| ())?;
    if metadata.file_type().is_symlink() {
        return Err(());
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return Err(());
        }
    }
    Ok(())
}

#[cfg(unix)]
fn restrict(path: &Path, directory: bool) -> Result<(), ()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(
        path,
        fs::Permissions::from_mode(if directory { 0o700 } else { 0o600 }),
    )
    .map_err(|_| ())
}

#[cfg(windows)]
fn restrict(path: &Path, directory: bool) -> Result<(), ()> {
    use std::{os::windows::process::CommandExt, process::Command};
    // A fresh protected DACL also removes explicit grants to other principals.
    // Only file metadata goes to this short-lived helper; no credential contents.
    const SCRIPT: &str = r#"
$ErrorActionPreference = 'Stop'
$account = [System.Security.Principal.WindowsIdentity]::GetCurrent().User
$system = [System.Security.Principal.SecurityIdentifier]::new('S-1-5-18')
if ($env:MPD_CREDENTIAL_DIRECTORY -eq '1') {
    $acl = [System.Security.AccessControl.DirectorySecurity]::new()
    $inherit = [System.Security.AccessControl.InheritanceFlags]'ContainerInherit, ObjectInherit'
} else {
    $acl = [System.Security.AccessControl.FileSecurity]::new()
    $inherit = [System.Security.AccessControl.InheritanceFlags]::None
}
$acl.SetAccessRuleProtection($true, $false)
foreach ($identity in @($account, $system)) {
    $rule = [System.Security.AccessControl.FileSystemAccessRule]::new($identity, [System.Security.AccessControl.FileSystemRights]::FullControl, $inherit, [System.Security.AccessControl.PropagationFlags]::None, [System.Security.AccessControl.AccessControlType]::Allow)
    $acl.AddAccessRule($rule)
}
if ($env:MPD_CREDENTIAL_DIRECTORY -eq '1') {
    [System.IO.Directory]::SetAccessControl($env:MPD_CREDENTIAL_FILE, $acl)
} else {
    [System.IO.File]::SetAccessControl($env:MPD_CREDENTIAL_FILE, $acl)
}
"#;
    let system = std::env::var_os("SystemRoot").ok_or(())?;
    let output = Command::new(
        Path::new(&system)
            .join("System32")
            .join("WindowsPowerShell")
            .join("v1.0")
            .join("powershell.exe"),
    )
    .args([
        "-NoLogo",
        "-NoProfile",
        "-NonInteractive",
        "-Command",
        SCRIPT,
    ])
    .env("MPD_CREDENTIAL_FILE", path)
    .env(
        "MPD_CREDENTIAL_DIRECTORY",
        if directory { "1" } else { "0" },
    )
    .creation_flags(0x08000000)
    .output()
    .map_err(|_| ())?;
    if output.status.success() {
        Ok(())
    } else {
        Err(())
    }
}
fn prepare_parent(path: &Path) -> Result<(), ()> {
    let parent = path.parent().ok_or(())?;
    fs::create_dir_all(parent).map_err(|_| ())?;
    reject_link(parent)?;
    restrict(parent, true)
}

pub fn read(path: &Path) -> Result<Option<Vec<u8>>, ()> {
    read_bounded(path, MAX_BYTES)
}

pub fn read_bounded(path: &Path, max_bytes: u64) -> Result<Option<Vec<u8>>, ()> {
    match fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(()),
        Ok(metadata) if !metadata.is_file() || metadata.len() > max_bytes => return Err(()),
        Ok(_) => {}
    }
    reject_link(path)?;
    prepare_parent(path)?;
    restrict(path, false)?;
    let file = fs::File::open(path).map_err(|_| ())?;
    let mut bytes = Vec::new();
    file.take(max_bytes.checked_add(1).ok_or(())?)
        .read_to_end(&mut bytes)
        .map_err(|_| ())?;
    if bytes.len() as u64 > max_bytes {
        return Err(());
    }
    Ok(Some(bytes))
}

pub fn write(path: &Path, bytes: &[u8]) -> Result<(), ()> {
    write_bounded(path, bytes, MAX_BYTES)
}

pub fn write_bounded(path: &Path, bytes: &[u8], max_bytes: u64) -> Result<(), ()> {
    if bytes.len() as u64 > max_bytes {
        return Err(());
    }
    prepare_parent(path)?;
    if fs::symlink_metadata(path).is_ok() {
        reject_link(path)?;
    }
    let mut temporary =
        tempfile::NamedTempFile::new_in(path.parent().ok_or(())?).map_err(|_| ())?;
    restrict(temporary.path(), false)?;
    temporary.write_all(bytes).map_err(|_| ())?;
    temporary.as_file().sync_all().map_err(|_| ())?;
    temporary.persist(path).map_err(|_| ())?;
    Ok(())
}

pub fn delete(path: &Path) -> Result<(), ()> {
    match fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err(()),
        Ok(_) => {}
    }
    reject_link(path)?;
    prepare_parent(path)?;
    fs::remove_file(path).map_err(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn private_atomic_file_round_trip_and_size_limit() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("credentials/test.json");
        assert!(read(&path).unwrap().is_none());
        write(&path, b"first").unwrap();
        write(&path, b"replacement").unwrap();
        assert_eq!(read(&path).unwrap().unwrap(), b"replacement");
        assert!(write(&path, &vec![b'x'; MAX_BYTES as usize + 1]).is_err());
        assert_eq!(read(&path).unwrap().unwrap(), b"replacement");
        let larger = vec![b'y'; MAX_BYTES as usize + 1];
        write_bounded(&path, &larger, MAX_BYTES * 2).unwrap();
        assert_eq!(read_bounded(&path, MAX_BYTES * 2).unwrap().unwrap(), larger);
        assert!(read(&path).is_err());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert_eq!(
                fs::metadata(path.parent().unwrap())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
        }
        delete(&path).unwrap();
        assert!(read(&path).unwrap().is_none());
    }
    #[cfg(windows)]
    #[test]
    fn windows_permissions_remove_explicit_grants_to_other_accounts() {
        use std::{os::windows::process::CommandExt, process::Command};
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("credentials/test.json");
        write(&path, b"synthetic-only").unwrap();
        let run = |script: &str| {
            Command::new(
                Path::new(&std::env::var_os("SystemRoot").unwrap())
                    .join("System32")
                    .join("WindowsPowerShell")
                    .join("v1.0")
                    .join("powershell.exe"),
            )
            .args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                script,
            ])
            .env("MPD_CREDENTIAL_FILE", &path)
            .creation_flags(0x08000000)
            .output()
            .unwrap()
            .status
            .success()
        };
        assert!(run(r#"
$ErrorActionPreference = 'Stop'
$acl = [IO.File]::GetAccessControl($env:MPD_CREDENTIAL_FILE)
$everyone = [Security.Principal.SecurityIdentifier]::new('S-1-1-0')
$acl.AddAccessRule([Security.AccessControl.FileSystemAccessRule]::new($everyone, 'Read', 'Allow'))
[IO.File]::SetAccessControl($env:MPD_CREDENTIAL_FILE, $acl)
"#));
        assert_eq!(read(&path).unwrap().unwrap(), b"synthetic-only");
        assert!(run(r#"
$ErrorActionPreference = 'Stop'
$acl = [IO.File]::GetAccessControl($env:MPD_CREDENTIAL_FILE)
$allowed = @([Security.Principal.WindowsIdentity]::GetCurrent().User.Value, 'S-1-5-18')
$rules = $acl.GetAccessRules($true, $true, [Security.Principal.SecurityIdentifier])
if (-not $acl.AreAccessRulesProtected -or $rules.Count -ne 2) { exit 1 }
foreach ($rule in $rules) { if ($allowed -notcontains $rule.IdentityReference.Value -or $rule.IsInherited) { exit 1 } }
"#));
    }
    #[cfg(unix)]
    #[test]
    fn symlink_targets_are_never_read_or_replaced() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("untouched");
        fs::write(&target, b"original").unwrap();
        let linked = directory.path().join("linked");
        std::os::unix::fs::symlink(&target, &linked).unwrap();
        assert!(read(&linked).is_err());
        assert!(write(&linked, b"replacement").is_err());
        assert_eq!(fs::read(&target).unwrap(), b"original");
    }
}
