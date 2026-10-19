use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io;
use std::path::{Component, Path, PathBuf};

use flate2::read::GzDecoder;
use tar::{Archive, EntryType};

use super::invalid;

#[derive(Clone)]
struct Member {
    kind: EntryType,
    link: Option<PathBuf>,
    mode: u32,
}

fn member_path(path: &Path) -> io::Result<PathBuf> {
    if path.components().any(|p| {
        matches!(
            p,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) || !path.starts_with("terminal-browser")
    {
        return Err(invalid(format!(
            "unsafe tb archive path: {}",
            path.display()
        )));
    }
    Ok(path
        .components()
        .filter(|p| *p != Component::CurDir)
        .collect())
}

fn resolve_link(
    name: &Path,
    member: &Member,
    members: &HashMap<PathBuf, Member>,
) -> io::Result<PathBuf> {
    resolve_link_inner(name, member, members, &mut HashSet::new())
}

fn resolve_link_inner(
    name: &Path,
    member: &Member,
    members: &HashMap<PathBuf, Member>,
    seen: &mut HashSet<PathBuf>,
) -> io::Result<PathBuf> {
    if !seen.insert(name.to_path_buf()) {
        return Err(invalid("tb archive link cycle"));
    }
    let link = member
        .link
        .as_ref()
        .ok_or_else(|| invalid("missing archive link target"))?;
    let mut target = if member.kind.is_symlink() {
        name.parent()
            .ok_or_else(|| invalid("invalid link path"))?
            .to_path_buf()
    } else {
        PathBuf::new()
    };
    // Follow each symlink before processing a subsequent '..', exactly as the
    // filesystem does. Lexically normalizing first can hide an escaping link.
    for part in link.components() {
        match part {
            Component::Normal(p) => {
                target.push(p);
                if let Some(other) = members
                    .get(&target)
                    .filter(|m| m.kind.is_symlink() || m.kind.is_hard_link())
                {
                    target = resolve_link_inner(&target, other, members, seen)?;
                }
            }
            Component::CurDir => (),
            Component::ParentDir if target.components().count() > 1 => {
                target.pop();
            }
            _ => {
                return Err(invalid(format!(
                    "escaping tb archive link: {}",
                    name.display()
                )))
            }
        }
    }
    seen.remove(name);
    member_path(&target)?;
    if !members.contains_key(&target) {
        return Err(invalid("dangling tb archive link"));
    }
    Ok(target)
}

fn open_archive(path: &Path) -> io::Result<Archive<GzDecoder<File>>> {
    Ok(Archive::new(GzDecoder::new(File::open(path)?)))
}

pub(super) fn unpack(archive: &Path, destination: &Path) -> io::Result<()> {
    let mut members = HashMap::new();
    // Validate the complete archive before creating its destination or writing
    // payload bytes. Do not delegate path handling to tar's extraction API.
    for entry in open_archive(archive)?.entries()? {
        let entry = entry?;
        let name = member_path(&entry.path()?)?;
        let kind = entry.header().entry_type();
        if !(kind.is_dir() || kind.is_file() || kind.is_symlink() || kind.is_hard_link()) {
            return Err(invalid(
                "unsupported tb archive entry type (device, FIFO or extension)",
            ));
        }
        let member = Member {
            kind,
            link: entry.link_name()?.map(|p| p.into_owned()),
            mode: entry.header().mode()? & 0o777,
        };
        if members.insert(name, member).is_some() {
            return Err(invalid("duplicate tb archive path"));
        }
    }
    let mut resolved_links = HashMap::new();
    for (name, member) in &members {
        for parent in name.ancestors().skip(1) {
            if members.get(parent).is_some_and(|m| !m.kind.is_dir()) {
                return Err(invalid("tb archive entry traverses a non-directory"));
            }
        }
        if member.kind.is_symlink() || member.kind.is_hard_link() {
            let target = resolve_link(name, member, &members)?;
            if member.kind.is_hard_link() && !members.get(&target).is_some_and(|m| m.kind.is_file())
            {
                return Err(invalid("tb archive hard link must resolve to a file"));
            }
            resolved_links.insert(name.clone(), target);
        }
    }
    fs::create_dir(destination)?;
    for entry in open_archive(archive)?.entries()? {
        let mut entry = entry?;
        let name = member_path(&entry.path()?)?;
        let member = members
            .get(&name)
            .ok_or_else(|| invalid("tb archive changed while reading"))?;
        let path = destination.join(&name);
        if member.kind.is_dir() {
            fs::create_dir_all(&path)?;
        }
        if member.kind.is_file() {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut file = File::create_new(&path)?;
            io::copy(&mut entry, &mut file)?;
            fs::set_permissions(&path, crate::platform::tb_archive_permissions(member.mode)?)?;
        }
    }
    // All real directories/files exist before links. Entries cannot be written
    // through a link, irrespective of ordering in the archive.
    for (name, member) in &members {
        if member.kind.is_symlink() || member.kind.is_hard_link() {
            let path = destination.join(name);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            if member.kind.is_hard_link() {
                let target = resolved_links
                    .get(name)
                    .ok_or_else(|| invalid("unvalidated hard link"))?;
                fs::hard_link(destination.join(target), &path)?;
            } else {
                crate::platform::tb_file_symlink(
                    member
                        .link
                        .as_ref()
                        .ok_or_else(|| invalid("missing link target"))?,
                    &path,
                )?;
            }
        }
    }
    Ok(())
}
