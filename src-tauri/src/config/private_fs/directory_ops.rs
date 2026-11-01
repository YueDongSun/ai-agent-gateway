//! Identity-bound directory operations for recoverable same-volume cutovers.
//! Inspection never adopts existing ACLs or changes directory ownership.

use super::{
    failure, metadata, platform, reject_linked_existing_components, Managed_directory_identity,
};
use crate::error::AppError;
use std::path::Path;

fn validate_path(path: &Path) -> Result<(), AppError> {
    if !path.is_absolute()
        || path.components().any(|part| {
            matches!(
                part,
                std::path::Component::ParentDir | std::path::Component::CurDir
            )
        })
    {
        return Err(failure("directory_path_invalid"));
    }
    reject_linked_existing_components(path)
}

pub(crate) fn inspect_directory_object(
    path: &Path,
) -> Result<Managed_directory_identity, AppError> {
    validate_path(path)?;
    if metadata(path)?.is_none_or(|meta| !meta.is_dir()) {
        return Err(failure("directory_expected"));
    }
    Ok(platform::durable_identity(platform::directory_identity(
        path,
    )?))
}

pub(crate) fn rename_directory_object_no_replace(
    from: &Path,
    to: &Path,
    expected: Managed_directory_identity,
) -> Result<(), AppError> {
    validate_path(from)?;
    validate_path(to)?;
    if inspect_directory_object(from)? != expected {
        return Err(failure("directory_object_changed"));
    }
    if metadata(to)?.is_some() {
        return Err(failure("directory_destination_occupied"));
    }
    let parent = to.parent().ok_or_else(|| failure("parent_missing"))?;
    let parent_identity = inspect_directory_object(parent)?;
    if parent_identity.volume != expected.volume {
        return Err(failure("directory_cross_volume_refused"));
    }
    #[cfg(windows)]
    {
        // Locked objects prevent competing namespace changes. The final native
        // rename deliberately has ReplaceIfExists=false, including after a race.
        let source = platform::open_directory_guard(from, true)?;
        let destination_parent = platform::open_directory_guard(parent, false)?;
        if platform::durable_identity(platform::file_identity_from_file(&source)?) != expected
            || platform::durable_identity(platform::file_identity_from_file(&destination_parent)?)
                != parent_identity
        {
            return Err(failure("directory_object_changed"));
        }
        validate_path(from)?;
        validate_path(parent)?;
        platform::rename_mirror_handle(&source, to)?;
        if inspect_directory_object(to)? != expected
            || inspect_directory_object(parent)? != parent_identity
        {
            return Err(failure("directory_object_changed"));
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        // std::fs::rename can replace an empty destination on Unix. Do not
        // advertise no-replace behavior until its platform primitive is qualified.
        let _ = (from, to, expected, parent_identity);
        Err(failure("directory_rename_platform_unsupported"))
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn same_volume_directory_cutover_preserves_object_data_and_file_timestamps() {
        let fixture = super::super::private_temp_dir(&std::env::temp_dir()).unwrap();
        let source = fixture.path().join("source");
        let target = fixture.path().join("target");
        super::super::prepare_managed_private_dir(&source).unwrap();
        let transcript = source.join("session.jsonl");
        fs::write(&transcript, b"synthetic complete transcript\n").unwrap();
        let before = fs::metadata(&transcript).unwrap();
        let identity = inspect_directory_object(&source).unwrap();
        rename_directory_object_no_replace(&source, &target, identity).unwrap();
        assert!(!source.exists());
        assert_eq!(inspect_directory_object(&target).unwrap(), identity);
        assert_eq!(
            fs::read(target.join("session.jsonl")).unwrap(),
            b"synthetic complete transcript\n"
        );
        let after = fs::metadata(target.join("session.jsonl")).unwrap();
        assert_eq!(before.created().unwrap(), after.created().unwrap());
        assert_eq!(before.modified().unwrap(), after.modified().unwrap());
    }

    #[test]
    fn an_existing_destination_and_a_changed_source_are_never_replaced() {
        let fixture = super::super::private_temp_dir(&std::env::temp_dir()).unwrap();
        let source = fixture.path().join("source");
        let target = fixture.path().join("target");
        let foreign = fixture.path().join("foreign");
        for path in [&source, &target, &foreign] {
            super::super::prepare_managed_private_dir(path).unwrap();
        }
        fs::write(source.join("history"), b"source history").unwrap();
        fs::write(target.join("history"), b"independent target history").unwrap();
        let source_identity = inspect_directory_object(&source).unwrap();
        let target_identity = inspect_directory_object(&target).unwrap();
        assert!(rename_directory_object_no_replace(&source, &target, source_identity).is_err());
        assert!(rename_directory_object_no_replace(
            &source,
            &fixture.path().join("empty"),
            inspect_directory_object(&foreign).unwrap()
        )
        .is_err());
        assert_eq!(inspect_directory_object(&source).unwrap(), source_identity);
        assert_eq!(inspect_directory_object(&target).unwrap(), target_identity);
        assert_eq!(fs::read(source.join("history")).unwrap(), b"source history");
        assert_eq!(
            fs::read(target.join("history")).unwrap(),
            b"independent target history"
        );
    }
}
