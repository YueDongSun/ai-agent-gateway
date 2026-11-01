//! UTF-16 names for native filesystem calls without resolving filesystem links.
//! Shared by private storage and the MCP client-sync permission boundary.

use super::{failure, io_failure};
use crate::error::AppError;
use std::os::windows::ffi::OsStrExt;
use std::path::{Component, Path, Prefix};

// Rust's Windows filesystem calls also reserve space for directory creation.
const LEGACY_PATH_LIMIT: usize = 248;
const SEPARATOR: u16 = b'\\' as u16;
const QUESTION_MARK: u16 = b'?' as u16;

pub(crate) fn encode_native_path(path: &Path) -> Result<Vec<u16>, AppError> {
    let mut name: Vec<u16> = path.as_os_str().encode_wide().collect();
    if name.contains(&0) {
        return Err(failure("path_contains_nul"));
    }
    name.push(0);
    if name.len() < LEGACY_PATH_LIMIT
        || name.starts_with(&[SEPARATOR, SEPARATOR, QUESTION_MARK, SEPARATOR])
        || name.starts_with(&[SEPARATOR, QUESTION_MARK, QUESTION_MARK, SEPARATOR])
    {
        return Ok(name);
    }

    // Normalize separators and dot components before entering the verbatim
    // namespace; canonicalize would follow links and require an existing target.
    let absolute_path = std::path::absolute(path)
        .map_err(|error| io_failure("path_normalization_failed", &error))?;
    let (prefix, offset) = match absolute_path.components().next() {
        Some(Component::Prefix(prefix)) => match prefix.kind() {
            Prefix::Disk(_) => (r"\\?\", 0),
            Prefix::UNC(..) => (r"\\?\UNC\", 2),
            Prefix::DeviceNS(_) => (r"\\?\", 4),
            _ => ("", 0),
        },
        _ => ("", 0),
    };
    Ok(prefix
        .encode_utf16()
        .chain(absolute_path.as_os_str().encode_wide().skip(offset))
        .chain(Some(0))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;

    fn terminated(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(Some(0)).collect()
    }

    #[test]
    fn short_paths_are_not_reinterpreted() {
        for path in [r"C:\fixture\state.json", "C:/fixture/./state.json"] {
            assert_eq!(
                encode_native_path(Path::new(path)).unwrap(),
                terminated(path)
            );
        }
    }

    #[test]
    fn long_drive_path_normalizes_separators_and_dot_components() {
        let component = "nested".repeat(48);
        let path = format!("C:/fixture/{component}/./state.json");
        let expected = format!(r"\\?\C:\fixture\{component}\state.json");
        assert_eq!(
            encode_native_path(Path::new(&path)).unwrap(),
            terminated(&expected)
        );
    }

    #[test]
    fn long_unc_path_uses_verbatim_unc_prefix() {
        let component = "nested".repeat(48);
        let path = format!(r"\\server\share\{component}\state.json");
        let expected = format!(r"\\?\UNC\server\share\{component}\state.json");
        assert_eq!(
            encode_native_path(Path::new(&path)).unwrap(),
            terminated(&expected)
        );
    }

    #[test]
    fn existing_verbatim_and_nt_names_are_preserved() {
        let component = "nested".repeat(48);
        for prefix in [r"\\?\C:\", r"\\?\UNC\server\share\", r"\??\C:\"] {
            let path = format!("{prefix}{component}\\.\\state.json ");
            assert_eq!(
                encode_native_path(Path::new(&path)).unwrap(),
                terminated(&path)
            );
        }
    }

    #[test]
    fn long_device_namespace_matches_filesystem_verbatim_conversion() {
        let component = "nested".repeat(48);
        let path = format!(r"\\.\C:\fixture\{component}\state.json");
        let expected = format!(r"\\?\C:\fixture\{component}\state.json");
        assert_eq!(
            encode_native_path(Path::new(&path)).unwrap(),
            terminated(&expected)
        );
    }

    #[test]
    fn long_paths_preserve_unpaired_utf16_surrogates() {
        let mut units: Vec<u16> = r"C:\fixture\".encode_utf16().collect();
        units.extend(std::iter::repeat_n(b'x' as u16, LEGACY_PATH_LIMIT));
        units.push(0xd800);
        let path = OsString::from_wide(&units);
        let expected: Vec<u16> = r"\\?\".encode_utf16().chain(units).chain(Some(0)).collect();
        assert_eq!(encode_native_path(Path::new(&path)).unwrap(), expected);
    }

    #[test]
    fn legacy_length_boundary_includes_the_terminator() {
        for name_length in [LEGACY_PATH_LIMIT - 2, LEGACY_PATH_LIMIT - 1] {
            let path = format!(r"C:\{}", "x".repeat(name_length - 3));
            let expected = if name_length + 1 < LEGACY_PATH_LIMIT {
                terminated(&path)
            } else {
                terminated(&format!(r"\\?\{path}"))
            };
            assert_eq!(encode_native_path(Path::new(&path)).unwrap(), expected);
        }
    }

    #[test]
    fn nul_is_rejected_before_ffi() {
        for prefix in [r"C:\", r"\\?\C:\"] {
            let path = format!("{prefix}state\0.json");
            let error = encode_native_path(Path::new(&path)).unwrap_err();
            assert!(error
                .to_string()
                .contains("private_storage:path_contains_nul"));
        }
    }
}
