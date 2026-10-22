use super::invalid;
use flate2::read::GzDecoder;
use std::collections::HashSet;
use std::fs::{self, File};
use std::io;
use std::path::{Component, Path, PathBuf};
use tar::Archive;

fn member_path(path: &Path) -> io::Result<PathBuf> {
    if !path.starts_with("mac-dialog-decisions")
        || path.components().any(|p| {
            matches!(
                p,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(invalid(format!(
            "unsafe mac-dialog archive path: {}",
            path.display()
        )));
    }
    Ok(path
        .components()
        .filter(|p| *p != Component::CurDir)
        .collect())
}
fn open(path: &Path) -> io::Result<Archive<GzDecoder<File>>> {
    Ok(Archive::new(GzDecoder::new(File::open(path)?)))
}
pub(super) fn unpack(archive: &Path, destination: &Path) -> io::Result<()> {
    let mut names = HashSet::new();
    let mut files = HashSet::new();
    // The Swift app and dependency-free Node plugin contain regular files and
    // directories only. Reject links/devices before writing any payload bytes.
    for entry in open(archive)?.entries()? {
        let entry = entry?;
        let name = member_path(&entry.path()?)?;
        if !names.insert(name.clone()) {
            return Err(invalid("duplicate mac-dialog archive path"));
        }
        let kind = entry.header().entry_type();
        if kind.is_file() {
            files.insert(name);
        } else if !kind.is_dir() {
            return Err(invalid(
                "mac-dialog archive contains a link or unsupported entry type",
            ));
        }
    }
    for name in &names {
        if name
            .ancestors()
            .skip(1)
            .any(|parent| files.contains(parent))
        {
            return Err(invalid("mac-dialog archive traverses a non-directory"));
        }
    }
    fs::create_dir(destination)?;
    for entry in open(archive)?.entries()? {
        let mut entry = entry?;
        let name = member_path(&entry.path()?)?;
        let path = destination.join(name);
        if entry.header().entry_type().is_dir() {
            fs::create_dir_all(path)?;
        } else {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut file = File::create_new(&path)?;
            io::copy(&mut entry, &mut file)?;
            fs::set_permissions(
                path,
                crate::platform::tb_archive_permissions(entry.header().mode()? & 0o777)?,
            )?;
        }
    }
    Ok(())
}
