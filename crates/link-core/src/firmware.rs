//! Safe filesystem staging contracts for the official manual firmware workflow.

use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use rustix::fs::{Mode, OFlags, RenameFlags, openat, renameat_with};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::{ErrorKind, LinkError, paths::AppPaths};

pub const FIRMWARE_OPERATION_SCHEMA_VERSION: u32 = 1;
pub const OFFICIAL_FIRMWARE_FILENAME: &str = "Insta360LINK2CPROFW_HOST.bin";
pub const MINIMUM_FIRMWARE_BYTES: u64 = 16 * 1024 * 1024;
pub const MAXIMUM_FIRMWARE_BYTES: u64 = 256 * 1024 * 1024;
const COPY_CHUNK_BYTES: usize = 1024 * 1024;
const FREE_SPACE_MARGIN_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum FirmwareOperationState {
    Validating,
    AwaitingUDisk,
    AwaitingMount,
    Copying,
    Synchronized,
    AwaitingReconnect,
    Complete,
    Partial,
    Failed,
    Interrupted,
    DryRun,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FirmwareFileInfo {
    pub name: String,
    pub size_bytes: u64,
    pub sha256: String,
    pub expected_sha256: Option<String>,
    pub checksum_verified: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FirmwareVolumeIdentity {
    pub block_device: String,
    pub label: String,
    pub filesystem: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum FirmwareVersionComparison {
    Changed,
    Unchanged,
    Unavailable,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct FirmwareOperationError {
    pub code: String,
    pub message: String,
    pub details: Map<String, Value>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct FirmwareStageReport {
    pub schema_version: u32,
    pub operation_id: String,
    pub started_unix_ms: u128,
    pub updated_unix_ms: u128,
    pub completed_unix_ms: Option<u128>,
    pub state: FirmwareOperationState,
    pub topology: String,
    pub normal_stable_id: Option<String>,
    pub maintenance_stable_id: Option<String>,
    pub source: Option<FirmwareFileInfo>,
    pub volume: Option<FirmwareVolumeIdentity>,
    pub destination_name: Option<String>,
    pub bytes_copied: u64,
    pub synchronized: bool,
    pub pre_version: Option<String>,
    pub post_version: Option<String>,
    pub version_comparison: FirmwareVersionComparison,
    pub dry_run: bool,
    pub warning: String,
    pub guidance: Vec<String>,
    pub events: Vec<FirmwareStageEvent>,
    pub log_path: Option<PathBuf>,
    pub error: Option<FirmwareOperationError>,
}

impl FirmwareStageReport {
    #[must_use]
    pub fn new(
        operation_id: String,
        topology: String,
        normal_stable_id: Option<String>,
        pre_version: Option<String>,
        dry_run: bool,
    ) -> Self {
        let now = now_unix_ms();
        Self {
            schema_version: FIRMWARE_OPERATION_SCHEMA_VERSION,
            operation_id,
            started_unix_ms: now,
            updated_unix_ms: now,
            completed_unix_ms: None,
            state: FirmwareOperationState::Validating,
            topology,
            normal_stable_id,
            maintenance_stable_id: None,
            source: None,
            volume: None,
            destination_name: None,
            bytes_copied: 0,
            synchronized: false,
            pre_version,
            post_version: None,
            version_comparison: FirmwareVersionComparison::Unavailable,
            dry_run,
            warning: "Do not disconnect the USB cable or operate the camera while firmware is being copied or applied.".into(),
            guidance: Vec::new(),
            events: Vec::new(),
            log_path: None,
            error: None,
        }
    }

    pub fn transition(&mut self, state: FirmwareOperationState) {
        self.state = state;
        self.updated_unix_ms = now_unix_ms();
        if matches!(
            state,
            FirmwareOperationState::Complete
                | FirmwareOperationState::Partial
                | FirmwareOperationState::Failed
                | FirmwareOperationState::Interrupted
                | FirmwareOperationState::DryRun
        ) {
            self.completed_unix_ms = Some(self.updated_unix_ms);
        }
    }

    pub fn record_error(&mut self, state: FirmwareOperationState, error: &LinkError) {
        self.events.push(FirmwareStageEvent::new(
            state,
            error.message(),
            self.bytes_copied,
            self.source.as_ref().map(|source| source.size_bytes),
        ));
        self.error = Some(FirmwareOperationError {
            code: error.kind().code().into(),
            message: error.message().into(),
            details: error.details().clone(),
        });
        self.transition(state);
    }

    pub fn compare_versions(&mut self) {
        self.version_comparison = match (&self.pre_version, &self.post_version) {
            (Some(before), Some(after)) if before == after => FirmwareVersionComparison::Unchanged,
            (Some(_), Some(_)) => FirmwareVersionComparison::Changed,
            _ => FirmwareVersionComparison::Unavailable,
        };
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct FirmwareStageEvent {
    pub observed_unix_ms: u128,
    pub state: FirmwareOperationState,
    pub message: String,
    pub bytes_copied: u64,
    pub total_bytes: Option<u64>,
    pub percent: Option<u8>,
}

impl FirmwareStageEvent {
    #[must_use]
    pub fn new(
        state: FirmwareOperationState,
        message: impl Into<String>,
        bytes_copied: u64,
        total_bytes: Option<u64>,
    ) -> Self {
        let percent = total_bytes
            .filter(|total| *total > 0)
            .map(|total| ((bytes_copied.saturating_mul(100) / total).min(100)) as u8);
        Self {
            observed_unix_ms: now_unix_ms(),
            state,
            message: message.into(),
            bytes_copied,
            total_bytes,
            percent,
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct FirmwareWatchEvent {
    pub sequence: u64,
    pub observed_unix_ms: u128,
    pub kind: String,
    pub topology: String,
    pub stable_id: Option<String>,
    pub model: Option<String>,
    pub mode: Option<String>,
    pub volume: Option<FirmwareVolumeIdentity>,
    pub mounted: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FirmwareCopyResult {
    pub destination_name: String,
    pub bytes_copied: u64,
    pub sha256: String,
    pub synchronized: bool,
}

#[derive(Clone, Debug)]
pub struct FirmwareOperationStore {
    directory: PathBuf,
}

impl FirmwareOperationStore {
    #[must_use]
    pub fn new(directory: PathBuf) -> Self {
        Self { directory }
    }

    pub fn from_process() -> Result<Self, LinkError> {
        Ok(Self::new(AppPaths::from_process()?.state.join("firmware")))
    }

    #[must_use]
    pub fn path(&self, operation_id: &str) -> PathBuf {
        self.directory.join(format!("{operation_id}.json"))
    }

    pub fn write(&self, report: &FirmwareStageReport) -> Result<PathBuf, LinkError> {
        AppPaths::ensure_private(&self.directory)?;
        let path = self.path(&report.operation_id);
        let mut bytes = serde_json::to_vec_pretty(report).map_err(|error| {
            LinkError::new(
                ErrorKind::IoFailure,
                "failed to serialize firmware operation log",
            )
            .with_detail("reason", error.to_string())
        })?;
        bytes.push(b'\n');
        let mut temporary = tempfile::NamedTempFile::new_in(&self.directory).map_err(|error| {
            firmware_io_error(
                "failed to create temporary firmware operation log",
                &path,
                &error,
            )
        })?;
        temporary
            .as_file_mut()
            .write_all(&bytes)
            .and_then(|()| temporary.as_file().sync_all())
            .map_err(|error| {
                firmware_io_error("failed to write firmware operation log", &path, &error)
            })?;
        temporary.persist(&path).map_err(|error| {
            firmware_io_error(
                "failed to finalize firmware operation log",
                &path,
                &error.error,
            )
        })?;
        File::open(&self.directory)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| {
                firmware_io_error(
                    "failed to synchronize firmware operation directory",
                    &self.directory,
                    &error,
                )
            })?;
        Ok(path)
    }
}

pub fn validate_firmware_file(
    path: &Path,
    expected_sha256: Option<&str>,
) -> Result<FirmwareFileInfo, LinkError> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            firmware_validation_error("firmware path must have a UTF-8 filename", path)
        })?;
    if name != OFFICIAL_FIRMWARE_FILENAME {
        return Err(firmware_validation_error(
            "firmware filename does not match the Link 2C Pro official filename",
            path,
        )
        .with_detail("expected", OFFICIAL_FIRMWARE_FILENAME));
    }
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| firmware_io_error("failed to inspect firmware source", path, &error))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(firmware_validation_error(
            "firmware source must be a regular file and not a symbolic link",
            path,
        ));
    }
    if !(MINIMUM_FIRMWARE_BYTES..=MAXIMUM_FIRMWARE_BYTES).contains(&metadata.len()) {
        return Err(firmware_validation_error(
            "firmware source size is outside the accepted bounds",
            path,
        )
        .with_detail("size_bytes", metadata.len())
        .with_detail("minimum_bytes", MINIMUM_FIRMWARE_BYTES)
        .with_detail("maximum_bytes", MAXIMUM_FIRMWARE_BYTES));
    }
    let expected_sha256 = expected_sha256.map(normalize_sha256).transpose()?;
    let mut source = open_source(path)?;
    let sha256 = hash_reader(&mut source, path)?;
    if let Some(expected) = &expected_sha256
        && expected != &sha256
    {
        return Err(firmware_validation_error(
            "firmware source checksum does not match --sha256",
            path,
        )
        .with_detail("expected_sha256", expected.clone())
        .with_detail("observed_sha256", sha256));
    }
    Ok(FirmwareFileInfo {
        name: name.into(),
        size_bytes: metadata.len(),
        checksum_verified: expected_sha256.is_some(),
        expected_sha256,
        sha256,
    })
}

pub fn copy_firmware_to_volume<F>(
    source_path: &Path,
    mount_root: &Path,
    source_info: &FirmwareFileInfo,
    operation_id: &str,
    interrupted: &AtomicBool,
    mut progress: F,
) -> Result<FirmwareCopyResult, LinkError>
where
    F: FnMut(u64) -> Result<(), LinkError>,
{
    validate_firmware_destination(mount_root, source_info.size_bytes)?;

    let temporary_name = format!(
        ".{}.linkctl-{}.tmp",
        OFFICIAL_FIRMWARE_FILENAME, operation_id
    );
    let temporary_path = mount_root.join(&temporary_name);
    let directory = File::open(mount_root).map_err(|error| {
        firmware_io_error("failed to open firmware volume root", mount_root, &error)
    })?;
    let temporary_fd = openat(
        &directory,
        temporary_name.as_str(),
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW,
        Mode::RUSR | Mode::WUSR,
    )
    .map_err(|error| {
        firmware_io_error(
            "failed to create temporary firmware destination",
            &temporary_path,
            &error.into(),
        )
    })?;
    let mut temporary = File::from(temporary_fd);
    let copy_result = (|| {
        let mut source = open_source(source_path)?;
        let mut hasher = Sha256::new();
        let mut buffer = vec![0_u8; COPY_CHUNK_BYTES];
        let mut copied = 0_u64;
        loop {
            if interrupted.load(Ordering::SeqCst) {
                return Err(LinkError::new(
                    ErrorKind::FirmwareValidationFailure,
                    "firmware staging was interrupted before synchronization",
                )
                .with_detail("bytes_copied", copied));
            }
            let read = source.read(&mut buffer).map_err(|error| {
                firmware_io_error("failed to read firmware source", source_path, &error)
            })?;
            if read == 0 {
                break;
            }
            temporary.write_all(&buffer[..read]).map_err(|error| {
                firmware_io_error(
                    "failed to copy firmware to the U-Disk",
                    &temporary_path,
                    &error,
                )
            })?;
            hasher.update(&buffer[..read]);
            copied = copied.saturating_add(read as u64);
            progress(copied)?;
        }
        let copied_sha256 = format_digest(hasher.finalize());
        if copied != source_info.size_bytes || copied_sha256 != source_info.sha256 {
            return Err(firmware_validation_error(
                "firmware source changed after validation",
                source_path,
            )
            .with_detail("expected_bytes", source_info.size_bytes)
            .with_detail("observed_bytes", copied)
            .with_detail("expected_sha256", source_info.sha256.clone())
            .with_detail("observed_sha256", copied_sha256));
        }
        temporary.sync_all().map_err(|error| {
            firmware_io_error(
                "failed to synchronize temporary firmware file",
                &temporary_path,
                &error,
            )
        })?;
        drop(temporary);
        renameat_with(
            &directory,
            temporary_name.as_str(),
            &directory,
            OFFICIAL_FIRMWARE_FILENAME,
            RenameFlags::NOREPLACE,
        )
        .map_err(|error| {
            if error == rustix::io::Errno::EXIST {
                firmware_validation_error(
                    "firmware destination appeared during staging; refusing to overwrite it",
                    &mount_root.join(OFFICIAL_FIRMWARE_FILENAME),
                )
            } else {
                firmware_io_error(
                    "failed to finalize firmware destination",
                    &mount_root.join(OFFICIAL_FIRMWARE_FILENAME),
                    &error.into(),
                )
            }
        })?;
        let destination = mount_root.join(OFFICIAL_FIRMWARE_FILENAME);
        directory.sync_all().map_err(|error| {
            LinkError::new(
                ErrorKind::PartialSuccess,
                "firmware destination was finalized, but volume directory synchronization failed",
            )
            .with_detail("destination", destination.display().to_string())
            .with_detail("bytes_copied", copied)
            .with_detail("destination_finalized", true)
            .with_detail("synchronized", false)
            .with_detail("reason", error.to_string())
        })?;
        let mut staged = open_source(&destination).map_err(|error| {
            post_sync_verification_error(
                "staged firmware could not be reopened for checksum verification",
                &destination,
                copied,
                &error,
            )
        })?;
        let staged_sha256 = hash_reader(&mut staged, &destination).map_err(|error| {
            post_sync_verification_error(
                "staged firmware could not be read for checksum verification",
                &destination,
                copied,
                &error,
            )
        })?;
        if staged_sha256 != source_info.sha256 {
            return Err(LinkError::new(
                ErrorKind::PartialSuccess,
                "staged firmware checksum did not match after synchronization",
            )
            .with_detail("destination", destination.display().to_string())
            .with_detail("bytes_copied", copied)
            .with_detail("destination_finalized", true)
            .with_detail("synchronized", true)
            .with_detail("expected_sha256", source_info.sha256.clone())
            .with_detail("observed_sha256", staged_sha256));
        }
        Ok(FirmwareCopyResult {
            destination_name: OFFICIAL_FIRMWARE_FILENAME.into(),
            bytes_copied: copied,
            sha256: staged_sha256,
            synchronized: true,
        })
    })();
    if copy_result.is_err() && temporary_path.exists() {
        let _cleanup_error = fs::remove_file(&temporary_path);
        let _sync_error = directory.sync_all();
    }
    copy_result
}

pub fn validate_firmware_destination(
    mount_root: &Path,
    firmware_size_bytes: u64,
) -> Result<(), LinkError> {
    validate_mount_root(mount_root)?;
    reject_existing_destination(mount_root)?;
    reject_abandoned_temporaries(mount_root)?;
    let available = available_bytes(mount_root)?;
    let required = firmware_size_bytes.saturating_add(FREE_SPACE_MARGIN_BYTES);
    if available < required {
        return Err(firmware_validation_error(
            "firmware volume has insufficient free space",
            mount_root,
        )
        .with_detail("available_bytes", available)
        .with_detail("required_bytes", required));
    }
    Ok(())
}

#[must_use]
pub fn new_firmware_operation_id(topology: &str) -> String {
    let now = now_unix_ms();
    let digest = Sha256::digest(format!("{topology}:{now}:{}", std::process::id()));
    format!(
        "fw-{now}-{}",
        digest[..6]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    )
}

fn normalize_sha256(value: &str) -> Result<String, LinkError> {
    let normalized = value.trim().to_ascii_lowercase();
    if normalized.len() != 64 || !normalized.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(LinkError::new(
            ErrorKind::FirmwareValidationFailure,
            "--sha256 must be exactly 64 hexadecimal characters",
        )
        .with_detail("sha256", value.to_owned()));
    }
    Ok(normalized)
}

fn open_source(path: &Path) -> Result<File, LinkError> {
    OpenOptions::new()
        .read(true)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(path)
        .map_err(|error| firmware_io_error("failed to open firmware file", path, &error))
}

fn hash_reader(reader: &mut File, path: &Path) -> Result<String, LinkError> {
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; COPY_CHUNK_BYTES];
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|error| firmware_io_error("failed to hash firmware file", path, &error))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format_digest(hasher.finalize()))
}

fn format_digest(digest: impl AsRef<[u8]>) -> String {
    digest
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn validate_mount_root(path: &Path) -> Result<(), LinkError> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        firmware_io_error("failed to inspect firmware volume root", path, &error)
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(firmware_validation_error(
            "firmware volume root must be a real directory",
            path,
        ));
    }
    Ok(())
}

fn reject_existing_destination(root: &Path) -> Result<(), LinkError> {
    let destination = root.join(OFFICIAL_FIRMWARE_FILENAME);
    match fs::symlink_metadata(&destination) {
        Ok(_) => Err(firmware_validation_error(
            "firmware destination already exists; refusing to overwrite it",
            &destination,
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(firmware_io_error(
            "failed to inspect firmware destination",
            &destination,
            &error,
        )),
    }
}

fn reject_abandoned_temporaries(root: &Path) -> Result<(), LinkError> {
    let prefix = format!(".{OFFICIAL_FIRMWARE_FILENAME}.linkctl-");
    let entries = fs::read_dir(root).map_err(|error| {
        firmware_io_error("failed to inspect firmware volume root", root, &error)
    })?;
    for entry in entries {
        let entry = entry.map_err(|error| {
            firmware_io_error("failed to inspect firmware volume entry", root, &error)
        })?;
        if entry.file_name().to_string_lossy().starts_with(&prefix) {
            return Err(firmware_validation_error(
                "an incomplete linkctl firmware staging file is present",
                &entry.path(),
            ));
        }
    }
    Ok(())
}

fn available_bytes(path: &Path) -> Result<u64, LinkError> {
    let status = rustix::fs::statvfs(path).map_err(|error| {
        firmware_io_error(
            "failed to inspect firmware volume free space",
            path,
            &error.into(),
        )
    })?;
    Ok(status.f_bavail.saturating_mul(status.f_frsize))
}

fn firmware_validation_error(message: &'static str, path: &Path) -> LinkError {
    LinkError::new(ErrorKind::FirmwareValidationFailure, message)
        .with_detail("path", path.display().to_string())
}

fn post_sync_verification_error(
    message: &'static str,
    destination: &Path,
    bytes_copied: u64,
    cause: &LinkError,
) -> LinkError {
    LinkError::new(ErrorKind::PartialSuccess, message)
        .with_detail("destination", destination.display().to_string())
        .with_detail("bytes_copied", bytes_copied)
        .with_detail("destination_finalized", true)
        .with_detail("synchronized", true)
        .with_detail("cause_code", cause.kind().code())
        .with_detail("cause", cause.message().to_owned())
}

fn firmware_io_error(message: &'static str, path: &Path, error: &std::io::Error) -> LinkError {
    let kind = if error.kind() == std::io::ErrorKind::PermissionDenied {
        ErrorKind::PermissionDenied
    } else {
        ErrorKind::IoFailure
    };
    LinkError::new(kind, message)
        .with_detail("path", path.display().to_string())
        .with_detail("reason", error.to_string())
}

fn now_unix_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        os::unix::fs::{PermissionsExt, symlink},
        sync::atomic::AtomicBool,
    };

    use super::{
        FirmwareOperationState, FirmwareOperationStore, FirmwareStageReport,
        MAXIMUM_FIRMWARE_BYTES, MINIMUM_FIRMWARE_BYTES, OFFICIAL_FIRMWARE_FILENAME,
        copy_firmware_to_volume, validate_firmware_destination, validate_firmware_file,
    };

    fn firmware_file(root: &std::path::Path) -> std::path::PathBuf {
        let path = root.join(OFFICIAL_FIRMWARE_FILENAME);
        let file = fs::File::create(&path).unwrap();
        file.set_len(MINIMUM_FIRMWARE_BYTES).unwrap();
        path
    }

    #[test]
    fn firmware_file_validation_is_exact_and_hashes_the_source() {
        let directory = tempfile::tempdir().unwrap();
        let path = firmware_file(directory.path());
        let report = validate_firmware_file(&path, None).unwrap();
        assert_eq!(report.name, OFFICIAL_FIRMWARE_FILENAME);
        assert_eq!(report.size_bytes, MINIMUM_FIRMWARE_BYTES);
        assert_eq!(report.sha256.len(), 64);

        let wrong = directory.path().join("other.bin");
        fs::rename(&path, &wrong).unwrap();
        assert!(validate_firmware_file(&wrong, None).is_err());
    }

    #[test]
    fn firmware_validation_rejects_symlinks_sizes_and_checksum_mismatches() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("target.bin");
        fs::write(&target, b"not firmware").unwrap();
        let source = directory.path().join(OFFICIAL_FIRMWARE_FILENAME);
        symlink(&target, &source).unwrap();
        assert!(validate_firmware_file(&source, None).is_err());
        fs::remove_file(&source).unwrap();

        let file = fs::File::create(&source).unwrap();
        file.set_len(MAXIMUM_FIRMWARE_BYTES + 1).unwrap();
        assert!(validate_firmware_file(&source, None).is_err());
        file.set_len(MINIMUM_FIRMWARE_BYTES).unwrap();
        assert!(validate_firmware_file(&source, Some(&"f".repeat(64))).is_err());
    }

    #[test]
    fn copy_is_no_clobber_and_hash_verified() {
        let source_directory = tempfile::tempdir().unwrap();
        let volume = tempfile::tempdir().unwrap();
        let source = firmware_file(source_directory.path());
        let info = validate_firmware_file(&source, None).unwrap();
        let mut progress = Vec::new();
        let copied = copy_firmware_to_volume(
            &source,
            volume.path(),
            &info,
            "test-operation",
            &AtomicBool::new(false),
            |bytes| {
                progress.push(bytes);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(copied.bytes_copied, info.size_bytes);
        assert_eq!(copied.sha256, info.sha256);
        assert!(copied.synchronized);
        assert!(progress.windows(2).all(|values| values[0] < values[1]));
        assert!(volume.path().join(OFFICIAL_FIRMWARE_FILENAME).is_file());
        assert!(
            copy_firmware_to_volume(
                &source,
                volume.path(),
                &info,
                "duplicate",
                &AtomicBool::new(false),
                |_| Ok(()),
            )
            .is_err()
        );
    }

    #[test]
    fn interrupted_copy_removes_its_temporary_file() {
        let source_directory = tempfile::tempdir().unwrap();
        let volume = tempfile::tempdir().unwrap();
        let source = firmware_file(source_directory.path());
        let info = validate_firmware_file(&source, None).unwrap();
        let interrupted = AtomicBool::new(true);

        assert!(
            copy_firmware_to_volume(
                &source,
                volume.path(),
                &info,
                "interrupted",
                &interrupted,
                |_| Ok(()),
            )
            .is_err()
        );
        assert_eq!(fs::read_dir(volume.path()).unwrap().count(), 0);
    }

    #[test]
    fn abandoned_temporary_file_blocks_staging() {
        let volume = tempfile::tempdir().unwrap();
        fs::write(
            volume
                .path()
                .join(format!(".{OFFICIAL_FIRMWARE_FILENAME}.linkctl-old.tmp")),
            b"partial",
        )
        .unwrap();

        assert!(validate_firmware_destination(volume.path(), MINIMUM_FIRMWARE_BYTES).is_err());
    }

    #[test]
    fn destination_and_mount_symlinks_are_rejected() {
        let volume = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let outside_file = outside.path().join("outside.bin");
        fs::write(&outside_file, b"unchanged").unwrap();
        symlink(
            &outside_file,
            volume.path().join(OFFICIAL_FIRMWARE_FILENAME),
        )
        .unwrap();
        assert!(validate_firmware_destination(volume.path(), MINIMUM_FIRMWARE_BYTES).is_err());
        assert_eq!(fs::read(&outside_file).unwrap(), b"unchanged");

        let root_link = outside.path().join("volume-link");
        symlink(volume.path(), &root_link).unwrap();
        assert!(validate_firmware_destination(&root_link, MINIMUM_FIRMWARE_BYTES).is_err());
    }

    #[test]
    fn operation_logs_are_private_and_replace_atomically() {
        let directory = tempfile::tempdir().unwrap();
        let store = FirmwareOperationStore::new(directory.path().join("firmware"));
        let mut report = FirmwareStageReport::new(
            "fw-test".into(),
            "1-1".into(),
            Some("camera".into()),
            Some("v1".into()),
            false,
        );
        let path = store.write(&report).unwrap();
        report.transition(FirmwareOperationState::Complete);
        store.write(&report).unwrap();
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
