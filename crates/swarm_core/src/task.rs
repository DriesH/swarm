//! The pure parts of making a task: names, ports and copying files.

use std::path::{Path, PathBuf};

use crate::Error;
use crate::config::{self, PortRange};

const MAX_SLUG_CHARS: usize = 50;

pub(crate) const BRANCH_PREFIX: &str = "swarm/";

/// Lowercase letters and digits, with runs of anything else turned into one `-`.
pub fn slug(name: &str) -> Result<String, Error> {
    let mut slug = String::new();
    let mut pending_dash = false;

    for character in name.chars().flat_map(char::to_lowercase) {
        if character.is_ascii_lowercase() || character.is_ascii_digit() {
            if pending_dash && !slug.is_empty() {
                slug.push('-');
            }
            pending_dash = false;
            slug.push(character);
        } else {
            pending_dash = true;
        }
    }

    slug.truncate(MAX_SLUG_CHARS);
    let slug = slug.trim_end_matches('-').to_owned();

    if slug.is_empty() {
        return Err(Error::Invalid(format!(
            "the task name {name:?} has no letters or digits"
        )));
    }

    Ok(slug)
}

/// The first of `slug`, `slug-2`, `slug-3`, … that `taken` does not refuse.
pub(crate) fn first_free_slug(
    slug: &str,
    mut taken: impl FnMut(&str) -> Result<bool, Error>,
) -> Result<String, Error> {
    let mut candidate = slug.to_owned();
    let mut number = 1_u32;

    while taken(&candidate)? {
        number += 1;
        candidate = format!("{slug}-{number}");
    }

    Ok(candidate)
}

/// The lowest block of `per_worktree` ports in the range that no `used` port touches.
pub fn allocate_ports(ports: &PortRange, used: &[u16]) -> Result<Vec<u16>, Error> {
    let (first, last) = ports.bounds().map_err(Error::Invalid)?;
    let size = u32::from(ports.per_worktree);

    let mut start = u32::from(first);
    while start + size - 1 <= u32::from(last) {
        let block = start..start + size;
        match used.iter().find(|port| block.contains(&u32::from(**port))) {
            None => {
                let block = block
                    .map(|port| u16::try_from(port).map_err(|_| Error::NoFreePorts))
                    .collect::<Result<_, _>>()?;

                return Ok(block);
            }
            Some(taken) => start = u32::from(*taken) + 1,
        }
    }

    Err(Error::NoFreePorts)
}

/// Copies each existing entry from `root` to the same place in `worktree`. Entries
/// that leave the project, or are not files, are skipped; a failed write is an error.
pub fn copy_files(root: &Path, worktree: &Path, entries: &[String]) -> Result<(), Error> {
    for entry in entries {
        match copy_file(root, worktree, entry) {
            Ok(true) => {}
            Ok(false) => log::info!("not copying {entry:?}: it does not exist in the project"),
            Err(error @ Error::Invalid(_)) => log::warn!("not copying {entry:?}: {error}"),
            Err(error) => return Err(error),
        }
    }

    Ok(())
}

/// Makes `folder` and returns its real path, which must lie inside `base`. Nothing is
/// created when a symlink on the way leads out.
pub(crate) fn create_folder_inside(base: &Path, folder: &Path) -> Result<PathBuf, Error> {
    let io_error = |source| Error::Io {
        action: format!("prepare the folder {}", folder.display()),
        source,
    };
    let outside = || {
        Error::Invalid(format!(
            "the folder {} leads outside {}",
            folder.display(),
            base.display()
        ))
    };

    let base = base.canonicalize().map_err(io_error)?;

    let mut existing = folder;
    while !existing.exists() {
        existing = existing.parent().ok_or_else(outside)?;
    }
    if !existing
        .canonicalize()
        .map_err(io_error)?
        .starts_with(&base)
    {
        return Err(outside());
    }

    std::fs::create_dir_all(folder).map_err(io_error)?;

    let real = folder.canonicalize().map_err(io_error)?;
    if !real.starts_with(&base) {
        return Err(outside());
    }

    Ok(real)
}

/// Returns false when the source does not exist.
fn copy_file(root: &Path, worktree: &Path, entry: &str) -> Result<bool, Error> {
    if !config::is_inside_repository(entry) {
        return Err(Error::Invalid(
            "the path is not inside the project".to_owned(),
        ));
    }

    let source = root.join(entry);
    let copy_error = |source_error| Error::Io {
        action: format!("copy {entry}"),
        source: source_error,
    };

    let resolved = match source.canonicalize() {
        Ok(resolved) => resolved,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(copy_error(error)),
    };
    let resolved_root = root.canonicalize().map_err(copy_error)?;
    if !resolved.starts_with(&resolved_root) {
        return Err(Error::Invalid(
            "the path leads outside the project".to_owned(),
        ));
    }
    if !resolved.is_file() {
        return Err(Error::Invalid("only files can be copied".to_owned()));
    }

    let destination: PathBuf = worktree.join(entry);
    if let Some(parent) = destination.parent() {
        create_folder_inside(worktree, parent)?;
    }
    if destination.is_symlink() {
        return Err(Error::Invalid(
            "the destination in the worktree is a symlink".to_owned(),
        ));
    }
    std::fs::copy(&resolved, &destination).map_err(copy_error)?;

    Ok(true)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;

    static NEXT_FOLDER: AtomicU32 = AtomicU32::new(0);

    fn temp_folder() -> PathBuf {
        let number = NEXT_FOLDER.fetch_add(1, Ordering::Relaxed);
        let folder =
            std::env::temp_dir().join(format!("swarm-core-task-{}-{number}", std::process::id()));
        std::fs::create_dir_all(&folder).expect("folder");

        folder
    }

    fn ports(range: &str, per_worktree: u16) -> PortRange {
        PortRange {
            range: range.to_owned(),
            per_worktree,
        }
    }

    #[test]
    fn a_name_becomes_a_slug() {
        assert_eq!(
            slug("Fix the Login  bug!").as_deref().ok(),
            Some("fix-the-login-bug")
        );
    }

    #[test]
    fn edge_characters_are_trimmed() {
        assert_eq!(
            slug("--Hello, World--").as_deref().ok(),
            Some("hello-world")
        );
    }

    #[test]
    fn a_slug_is_cut_at_fifty_characters() {
        let name = "a".repeat(80);

        assert_eq!(slug(&name).map(|slug| slug.len()).ok(), Some(50));
    }

    #[test]
    fn a_cut_slug_does_not_end_in_a_dash() {
        let name = format!("{} b", "a".repeat(49));

        assert_eq!(slug(&name).as_deref().ok(), Some("a".repeat(49).as_str()));
    }

    #[test]
    fn a_name_without_letters_is_an_error() {
        assert!(slug("!!! ???").is_err());
    }

    #[test]
    fn a_taken_slug_gets_a_number() {
        let taken = ["fix", "fix-2"];

        let free = first_free_slug("fix", |candidate| Ok(taken.contains(&candidate)));

        assert_eq!(free.ok().as_deref(), Some("fix-3"));
    }

    #[test]
    fn a_free_slug_stays_as_it_is() {
        let free = first_free_slug("fix", |_| Ok(false));

        assert_eq!(free.ok().as_deref(), Some("fix"));
    }

    #[test]
    fn the_first_block_starts_at_the_range_start() {
        let block = allocate_ports(&ports("5170-5199", 3), &[]);

        assert_eq!(block.ok(), Some(vec![5170, 5171, 5172]));
    }

    #[test]
    fn the_next_block_follows_the_used_ports() {
        let block = allocate_ports(&ports("5170-5199", 3), &[5170, 5171, 5172]);

        assert_eq!(block.ok(), Some(vec![5173, 5174, 5175]));
    }

    #[test]
    fn a_freed_block_in_the_middle_is_reused() {
        let used = [5170, 5171, 5172, 5176, 5177, 5178];

        let block = allocate_ports(&ports("5170-5199", 3), &used);

        assert_eq!(block.ok(), Some(vec![5173, 5174, 5175]));
    }

    #[test]
    fn no_free_block_is_an_error() {
        let result = allocate_ports(&ports("5170-5172", 3), &[5171]);

        assert!(matches!(result, Err(Error::NoFreePorts)));
    }

    #[test]
    fn a_range_up_to_the_last_port_works() {
        let block = allocate_ports(&ports("65534-65535", 2), &[]);

        assert_eq!(block.ok(), Some(vec![65534, 65535]));
    }

    #[test]
    fn an_existing_file_is_copied_into_a_new_folder() {
        let root = temp_folder();
        let worktree = temp_folder();
        std::fs::create_dir_all(root.join("config")).expect("folder");
        std::fs::write(root.join("config/local.env"), "A=1").expect("file");

        copy_files(&root, &worktree, &["config/local.env".to_owned()]).expect("copy");

        let copied = std::fs::read_to_string(worktree.join("config/local.env"));
        std::fs::remove_dir_all(&root).expect("cleanup");
        std::fs::remove_dir_all(&worktree).expect("cleanup");
        assert_eq!(copied.ok().as_deref(), Some("A=1"));
    }

    #[test]
    fn a_missing_file_is_skipped() {
        let root = temp_folder();
        let worktree = temp_folder();

        copy_files(&root, &worktree, &[".env".to_owned()]).expect("copy");

        let exists = worktree.join(".env").exists();
        std::fs::remove_dir_all(&root).expect("cleanup");
        std::fs::remove_dir_all(&worktree).expect("cleanup");
        assert!(!exists);
    }

    #[test]
    fn a_path_outside_the_project_is_refused() {
        let outer = temp_folder();
        let root = outer.join("repo");
        let worktree = outer.join("worktree");
        std::fs::create_dir_all(&root).expect("folder");
        std::fs::create_dir_all(&worktree).expect("folder");
        std::fs::write(outer.join("secret"), "x").expect("file");

        copy_files(&root, &worktree, &["../secret".to_owned()]).expect("copy");
        copy_files(
            &root,
            &worktree,
            &[outer.join("secret").display().to_string()],
        )
        .expect("copy");

        let leaked = worktree.join("secret").exists();
        std::fs::remove_dir_all(&outer).expect("cleanup");
        assert!(!leaked);
    }

    #[test]
    fn a_symlink_leading_outside_is_refused() {
        let outer = temp_folder();
        let root = outer.join("repo");
        let worktree = outer.join("worktree");
        std::fs::create_dir_all(&root).expect("folder");
        std::fs::create_dir_all(&worktree).expect("folder");
        std::fs::write(outer.join("secret"), "x").expect("file");
        std::os::unix::fs::symlink(outer.join("secret"), root.join("link")).expect("link");

        copy_files(&root, &worktree, &["link".to_owned()]).expect("copy");

        let copied = worktree.join("link").exists();
        std::fs::remove_dir_all(&outer).expect("cleanup");
        assert!(!copied);
    }

    #[test]
    fn a_symlinked_folder_in_the_worktree_is_not_written_through() {
        let outer = temp_folder();
        let root = outer.join("repo");
        let worktree = outer.join("worktree");
        let target = outer.join("target");
        for folder in [root.join("data"), worktree.clone(), target.clone()] {
            std::fs::create_dir_all(folder).expect("folder");
        }
        std::fs::write(root.join("data/key.env"), "x").expect("file");
        std::os::unix::fs::symlink(&target, worktree.join("data")).expect("link");

        let result = copy_files(&root, &worktree, &["data/key.env".to_owned()]);

        let leaked = target.join("key.env").exists();
        std::fs::remove_dir_all(&outer).expect("cleanup");
        assert!(result.is_ok());
        assert!(!leaked);
    }

    #[test]
    fn a_symlink_as_destination_is_not_written_through() {
        let outer = temp_folder();
        let root = outer.join("repo");
        let worktree = outer.join("worktree");
        std::fs::create_dir_all(&root).expect("folder");
        std::fs::create_dir_all(&worktree).expect("folder");
        std::fs::write(root.join(".env"), "new").expect("file");
        std::fs::write(outer.join("target"), "old").expect("file");
        std::os::unix::fs::symlink(outer.join("target"), worktree.join(".env")).expect("link");

        let result = copy_files(&root, &worktree, &[".env".to_owned()]);

        let target = std::fs::read_to_string(outer.join("target"));
        std::fs::remove_dir_all(&outer).expect("cleanup");
        assert!(result.is_ok());
        assert_eq!(target.ok().as_deref(), Some("old"));
    }

    #[test]
    fn a_destination_that_is_a_folder_is_an_error() {
        let root = temp_folder();
        let worktree = temp_folder();
        std::fs::write(root.join(".env"), "x").expect("file");
        std::fs::create_dir_all(worktree.join(".env")).expect("folder");

        let result = copy_files(&root, &worktree, &[".env".to_owned()]);

        std::fs::remove_dir_all(&root).expect("cleanup");
        std::fs::remove_dir_all(&worktree).expect("cleanup");
        assert!(matches!(result, Err(Error::Io { .. })));
    }
}
