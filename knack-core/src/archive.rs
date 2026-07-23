//! Skill tarball create/unpack, shared by the knack CLI (`knack pack`,
//! HTTP publish) and knack-registry (archive serving, `build-static`,
//! and the publish endpoint). One implementation so the bytes a client
//! uploads are exactly the bytes the registry would have produced
//! itself, and so checksum-based change detection agrees on both ends.
//!
//! Gated behind the `archive` cargo feature (default-on) because
//! `tar`/`flate2` have no business in wasm32 builds of knack-core.

use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow, bail};
use flate2::{Compression, write::GzEncoder};
use tar::{Builder, Header};

use crate::{collect_files, read_skill, validate_skill_metadata};

/// Package a skill directory into a deterministic gzip tarball.
///
/// The archive root is a single directory named after the skill's
/// *frontmatter* name (not the on-disk directory name — vendors
/// commonly use unprefixed directory names with brand-prefixed
/// frontmatter names, and the archive is the point where the
/// canonical name wins). Every entry gets fixed mode/mtime/uid/gid
/// so the same content always produces the same bytes, which keeps
/// checksums stable across hosts and rebuilds.
///
/// Validates metadata (name well-formed, description present) but
/// not the dir-name-matches-frontmatter invariant; callers that
/// require the strict form (e.g. `knack pack`) run `validate_skill`
/// first.
pub fn create_skill_archive(skill_dir: &Path) -> Result<Vec<u8>> {
    let skill = read_skill(skill_dir)?;
    validate_skill_metadata(&skill)?;

    let encoder = GzEncoder::new(Vec::new(), Compression::default());
    let mut archive = Builder::new(encoder);
    for file in collect_files(skill_dir)? {
        let relative = file.strip_prefix(skill_dir).with_context(|| {
            format!(
                "failed to make {} relative to {}",
                file.display(),
                skill_dir.display()
            )
        })?;
        let archive_name = Path::new(&skill.name).join(relative);
        append_file(&mut archive, &file, &archive_name)?;
    }
    archive.finish()?;
    let encoder = archive.into_inner()?;
    Ok(encoder.finish()?)
}

fn append_file(
    archive: &mut Builder<GzEncoder<Vec<u8>>>,
    source: &Path,
    archive_name: &Path,
) -> Result<()> {
    let mut file =
        File::open(source).with_context(|| format!("failed to open {}", source.display()))?;
    let metadata = file
        .metadata()
        .with_context(|| format!("failed to stat {}", source.display()))?;
    if !metadata.is_file() {
        bail!("not a file: {}", source.display());
    }

    let mut header = Header::new_gnu();
    header.set_size(metadata.len());
    header.set_mode(0o644);
    header.set_mtime(0);
    header.set_uid(0);
    header.set_gid(0);
    header.set_cksum();

    archive
        .append_data(&mut header, archive_name, &mut file)
        .with_context(|| format!("failed to archive {}", source.display()))?;
    Ok(())
}

/// Unpack a gzip skill tarball (as produced by [`create_skill_archive`]
/// / `knack pack`) into `dest` and return the extracted skill root.
///
/// tar-rs's `unpack` already refuses entries that would escape `dest`
/// (path traversal); on top of that we enforce the skill-archive
/// shape: exactly one top-level directory and nothing else. Callers
/// still validate the returned directory's contents (`read_skill`,
/// `validate_skill`) — this function only guarantees safe extraction
/// and the single-root layout.
pub fn unpack_skill_archive<R: Read>(reader: R, dest: &Path) -> Result<PathBuf> {
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(reader));
    archive
        .unpack(dest)
        .context("failed to unpack skill archive")?;

    let mut root: Option<PathBuf> = None;
    for entry in std::fs::read_dir(dest)
        .with_context(|| format!("failed to read unpacked archive at {}", dest.display()))?
    {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            bail!(
                "skill archive must contain a single top-level directory, \
                 found stray entry: {}",
                entry.path().display()
            );
        }
        if root.replace(entry.path()).is_some() {
            bail!("skill archive must contain exactly one top-level directory, found several");
        }
    }
    root.ok_or_else(|| anyhow!("skill archive is empty"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{checksum_dir, validate_skill};

    fn write_skill(dir: &Path, name: &str) {
        std::fs::create_dir_all(dir.join("references")).unwrap();
        std::fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: \"A test skill.\"\n---\n\n# {name}\n"),
        )
        .unwrap();
        std::fs::write(dir.join("references/notes.md"), "extra content\n").unwrap();
    }

    #[test]
    fn archive_round_trips_and_preserves_checksum() {
        let src = tempfile::tempdir().unwrap();
        let skill_dir = src.path().join("demo");
        write_skill(&skill_dir, "demo");

        let bytes = create_skill_archive(&skill_dir).unwrap();

        let out = tempfile::tempdir().unwrap();
        let root = unpack_skill_archive(std::io::Cursor::new(&bytes), out.path()).unwrap();
        assert_eq!(root.file_name().unwrap(), "demo");

        let skill = read_skill(&root).unwrap();
        validate_skill(&skill).unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("references/notes.md")).unwrap(),
            "extra content\n"
        );
        assert_eq!(
            checksum_dir(&skill_dir).unwrap(),
            checksum_dir(&root).unwrap()
        );
    }

    #[test]
    fn archive_root_uses_frontmatter_name_not_dirname() {
        // Vendors ship `skills/composition-patterns/` containing
        // `name: vercel-composition-patterns`; the archive renames on
        // the way out so installs land under the canonical name.
        let src = tempfile::tempdir().unwrap();
        let skill_dir = src.path().join("composition-patterns");
        write_skill(&skill_dir, "vendor-composition-patterns");

        let bytes = create_skill_archive(&skill_dir).unwrap();
        let out = tempfile::tempdir().unwrap();
        let root = unpack_skill_archive(std::io::Cursor::new(&bytes), out.path()).unwrap();
        assert_eq!(root.file_name().unwrap(), "vendor-composition-patterns");
    }

    #[test]
    fn create_rejects_invalid_metadata() {
        let src = tempfile::tempdir().unwrap();
        let skill_dir = src.path().join("bad");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: Bad Name\ndescription: \"x\"\n---\n",
        )
        .unwrap();
        assert!(create_skill_archive(&skill_dir).is_err());
    }

    #[test]
    fn unpack_rejects_multiple_top_level_directories() {
        let encoder = GzEncoder::new(Vec::new(), Compression::default());
        let mut archive = Builder::new(encoder);
        for dir in ["one", "two"] {
            let mut header = Header::new_gnu();
            header.set_size(0);
            header.set_mode(0o644);
            header.set_cksum();
            archive
                .append_data(
                    &mut header,
                    Path::new(dir).join("SKILL.md"),
                    std::io::empty(),
                )
                .unwrap();
        }
        archive.finish().unwrap();
        let bytes = archive.into_inner().unwrap().finish().unwrap();

        let out = tempfile::tempdir().unwrap();
        let err = unpack_skill_archive(std::io::Cursor::new(&bytes), out.path()).unwrap_err();
        assert!(err.to_string().contains("exactly one top-level directory"));
    }

    #[test]
    fn unpack_rejects_stray_top_level_files() {
        let encoder = GzEncoder::new(Vec::new(), Compression::default());
        let mut archive = Builder::new(encoder);
        let mut header = Header::new_gnu();
        header.set_size(0);
        header.set_mode(0o644);
        header.set_cksum();
        archive
            .append_data(&mut header, Path::new("stray.txt"), std::io::empty())
            .unwrap();
        archive.finish().unwrap();
        let bytes = archive.into_inner().unwrap().finish().unwrap();

        let out = tempfile::tempdir().unwrap();
        let err = unpack_skill_archive(std::io::Cursor::new(&bytes), out.path()).unwrap_err();
        assert!(err.to_string().contains("stray entry"));
    }
}
