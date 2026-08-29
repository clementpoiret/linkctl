//! Versioned, peer-authenticated local IPC boundary.

use std::{
    fs,
    io::{self, Read, Write},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    time::Duration,
};

use link_core::{ErrorKind, LinkError, paths::AppPaths};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;

/// Current IPC protocol major version.
pub const PROTOCOL_VERSION: u32 = 1;
/// Maximum accepted JSON frame size.
pub const MAX_JSON_BYTES: usize = 1024 * 1024;
/// Maximum accepted binary payload size.
pub const MAX_BINARY_BYTES: usize = 64 * 1024 * 1024;

/// Default daemon socket path for the current user.
pub fn socket_path() -> Result<PathBuf, LinkError> {
    if let Some(path) = std::env::var_os("LINKCTL_DAEMON_SOCKET").filter(|value| !value.is_empty())
    {
        return Ok(PathBuf::from(path));
    }
    Ok(AppPaths::from_process()?.runtime.join("linkd.sock"))
}

/// A request sent by one local CLI process.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RequestEnvelope {
    pub protocol_version: u32,
    pub request_id: u64,
    pub operation: Operation,
}

impl RequestEnvelope {
    #[must_use]
    pub const fn new(request_id: u64, operation: Operation) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            request_id,
            operation,
        }
    }
}

/// Supported daemon operations.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "operation", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Operation {
    Status,
    Reload,
    Shutdown,
    PipelineStatus,
    PipelineGraph,
    PipelineMetrics,
    ControlList,
    ControlGet {
        selector: String,
    },
    ControlSet {
        writes: Vec<StandardControlWrite>,
        raw: bool,
        clamp: bool,
        batched: bool,
        fallback_individual: bool,
        dry_run: bool,
    },
    ControlReset {
        selector: String,
        raw: bool,
        dry_run: bool,
    },
    VcamList,
    VcamStart {
        specification: VirtualCameraSpec,
    },
    VcamStatus {
        name: Option<String>,
    },
    VcamStop {
        name: Option<String>,
    },
    Snapshot {
        encoding: SnapshotEncoding,
    },
    RecordingStart {
        specification: RecordingSpec,
    },
    RecordingStatus,
    RecordingStop,
}

/// One user-facing standard-control value resolved and applied by the daemon actor.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StandardControlWrite {
    pub selector: String,
    pub value: String,
}

/// A virtual-camera output contract and its host transforms.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct VirtualCameraSpec {
    pub name: String,
    pub output_device: PathBuf,
    pub width: u32,
    pub height: u32,
    pub fps_numerator: u32,
    pub fps_denominator: u32,
    pub format: String,
    pub rotation: Rotation,
    pub horizontal_flip: bool,
    pub vertical_flip: bool,
    pub crop: Option<NormalizedCrop>,
    pub fit: FitMode,
    pub zoom: f64,
    pub frame_x: f64,
    pub frame_y: f64,
    pub text_overlay: Option<String>,
    pub image_overlay: Option<PathBuf>,
    pub privacy_frame: bool,
}

impl Default for VirtualCameraSpec {
    fn default() -> Self {
        Self {
            name: "clean".into(),
            output_device: PathBuf::from("/dev/video20"),
            width: 1920,
            height: 1080,
            fps_numerator: 30,
            fps_denominator: 1,
            format: "YUY2".into(),
            rotation: Rotation::None,
            horizontal_flip: false,
            vertical_flip: false,
            crop: None,
            fit: FitMode::Contain,
            zoom: 1.0,
            frame_x: 0.5,
            frame_y: 0.5,
            text_overlay: None,
            image_overlay: None,
            privacy_frame: false,
        }
    }
}

/// Clockwise rotation applied to a virtual output.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Rotation {
    #[default]
    None,
    Clockwise90,
    Rotate180,
    Counterclockwise90,
}

/// How an image is fitted into its output contract.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum FitMode {
    #[default]
    Contain,
    Cover,
    Stretch,
}

/// A crop rectangle expressed in normalized source coordinates.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NormalizedCrop {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// Snapshot encoding returned in the binary response payload.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SnapshotEncoding {
    Jpeg,
    Png,
}

/// Background recording request.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RecordingSpec {
    pub output: PathBuf,
    pub container: RecordingContainer,
    pub overwrite: bool,
}

/// Supported daemon recording containers.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RecordingContainer {
    Matroska,
    Mp4,
}

/// One daemon response. Binary data follows the JSON frame when `binary_length` is non-zero.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResponseEnvelope {
    pub protocol_version: u32,
    pub request_id: u64,
    pub result: Result<Value, RemoteError>,
    pub binary_length: u64,
}

/// Serializable error returned by the daemon.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteError {
    pub kind: ErrorKind,
    pub message: String,
    pub details: serde_json::Map<String, Value>,
}

impl From<&LinkError> for RemoteError {
    fn from(error: &LinkError) -> Self {
        Self {
            kind: error.kind(),
            message: error.message().to_owned(),
            details: error.details().clone(),
        }
    }
}

impl RemoteError {
    #[must_use]
    pub fn into_link_error(self) -> LinkError {
        let mut error = LinkError::new(self.kind, self.message);
        for (key, value) in self.details {
            error = error.with_detail(key, value);
        }
        error
    }
}

/// Successful response including an optional binary body.
#[derive(Clone, Debug)]
pub struct ClientResponse {
    pub value: Value,
    pub binary: Vec<u8>,
}

/// Blocking local client. Each request uses a short-lived authenticated connection.
#[derive(Clone, Debug)]
pub struct Client {
    socket: PathBuf,
    timeout: Duration,
}

impl Client {
    #[must_use]
    pub fn new(socket: PathBuf, timeout: Duration) -> Self {
        Self { socket, timeout }
    }

    pub fn connect_default(timeout: Duration) -> Result<Self, LinkError> {
        Ok(Self::new(socket_path()?, timeout))
    }

    #[must_use]
    pub fn socket(&self) -> &Path {
        &self.socket
    }

    pub fn is_available(&self) -> bool {
        self.request(Operation::Status).is_ok()
    }

    pub fn request(&self, operation: Operation) -> Result<ClientResponse, LinkError> {
        let mut stream = UnixStream::connect(&self.socket)
            .map_err(|error| ipc_io_error("failed to connect to linkd", &self.socket, &error))?;
        stream
            .set_read_timeout(Some(self.timeout))
            .map_err(|error| {
                ipc_io_error("failed to set daemon read timeout", &self.socket, &error)
            })?;
        stream
            .set_write_timeout(Some(self.timeout))
            .map_err(|error| {
                ipc_io_error("failed to set daemon write timeout", &self.socket, &error)
            })?;
        verify_peer_uid(&stream)?;
        let request = RequestEnvelope::new(next_request_id(), operation);
        write_message(&mut stream, &request, &[])?;
        let (response, binary): (ResponseEnvelope, Vec<u8>) = read_message(&mut stream)?;
        if response.protocol_version != PROTOCOL_VERSION {
            return Err(protocol_mismatch(response.protocol_version));
        }
        if response.request_id != request.request_id {
            return Err(LinkError::new(
                ErrorKind::DaemonUnavailable,
                "daemon response request ID did not match",
            ));
        }
        if response.binary_length != binary.len() as u64 {
            return Err(LinkError::new(
                ErrorKind::DaemonUnavailable,
                "daemon response binary length did not match its envelope",
            ));
        }
        match response.result {
            Ok(value) => Ok(ClientResponse { value, binary }),
            Err(error) => Err(error.into_link_error()),
        }
    }
}

/// Read one bounded IPC message and its optional binary body.
pub fn read_message<T: DeserializeOwned>(
    reader: &mut impl Read,
) -> Result<(T, Vec<u8>), LinkError> {
    let json_length = read_u32(reader)? as usize;
    if json_length == 0 || json_length > MAX_JSON_BYTES {
        return Err(LinkError::new(
            ErrorKind::DaemonUnavailable,
            "invalid daemon JSON frame length",
        )
        .with_detail("length", json_length as u64));
    }
    let mut json = vec![0; json_length];
    reader.read_exact(&mut json).map_err(frame_io_error)?;
    let binary_length = read_u64(reader)? as usize;
    if binary_length > MAX_BINARY_BYTES {
        return Err(LinkError::new(
            ErrorKind::DaemonUnavailable,
            "daemon binary frame exceeds the size limit",
        )
        .with_detail("length", binary_length as u64));
    }
    let value = serde_json::from_slice(&json).map_err(|error| {
        LinkError::new(ErrorKind::DaemonUnavailable, "invalid daemon JSON frame")
            .with_detail("reason", error.to_string())
    })?;
    let mut binary = vec![0; binary_length];
    reader.read_exact(&mut binary).map_err(frame_io_error)?;
    Ok((value, binary))
}

/// Write one bounded IPC message and its optional binary body.
pub fn write_message<T: Serialize>(
    writer: &mut impl Write,
    value: &T,
    binary: &[u8],
) -> Result<(), LinkError> {
    let json = serde_json::to_vec(value).map_err(|error| {
        LinkError::new(
            ErrorKind::DaemonUnavailable,
            "failed to encode daemon JSON frame",
        )
        .with_detail("reason", error.to_string())
    })?;
    if json.len() > MAX_JSON_BYTES || binary.len() > MAX_BINARY_BYTES {
        return Err(LinkError::new(
            ErrorKind::DaemonUnavailable,
            "daemon IPC frame exceeds the size limit",
        ));
    }
    writer
        .write_all(&(json.len() as u32).to_be_bytes())
        .and_then(|()| writer.write_all(&json))
        .and_then(|()| writer.write_all(&(binary.len() as u64).to_be_bytes()))
        .and_then(|()| writer.write_all(binary))
        .and_then(|()| writer.flush())
        .map_err(frame_io_error)
}

/// Require the connected process to have the daemon user's UID.
pub fn verify_peer_uid(stream: &UnixStream) -> Result<(), LinkError> {
    let credentials = rustix::net::sockopt::socket_peercred(stream).map_err(|error| {
        LinkError::new(
            ErrorKind::DaemonUnavailable,
            "failed to inspect Unix peer credentials",
        )
        .with_detail("reason", error.to_string())
    })?;
    let expected = rustix::process::getuid();
    if credentials.uid != expected {
        return Err(LinkError::new(
            ErrorKind::PermissionDenied,
            "daemon IPC peer belongs to another user",
        )
        .with_detail("expected_uid", expected.as_raw() as u64)
        .with_detail("peer_uid", credentials.uid.as_raw() as u64));
    }
    Ok(())
}

/// Reject an incompatible protocol before dispatching an operation.
pub fn validate_protocol(version: u32) -> Result<(), LinkError> {
    if version == PROTOCOL_VERSION {
        Ok(())
    } else {
        Err(protocol_mismatch(version))
    }
}

fn protocol_mismatch(observed: u32) -> LinkError {
    LinkError::new(
        ErrorKind::DaemonUnavailable,
        "linkctl and linkd use incompatible IPC protocol versions",
    )
    .with_detail("supported_protocol", PROTOCOL_VERSION as u64)
    .with_detail("observed_protocol", observed as u64)
}

fn read_u32(reader: &mut impl Read) -> Result<u32, LinkError> {
    let mut bytes = [0; 4];
    reader.read_exact(&mut bytes).map_err(frame_io_error)?;
    Ok(u32::from_be_bytes(bytes))
}

fn read_u64(reader: &mut impl Read) -> Result<u64, LinkError> {
    let mut bytes = [0; 8];
    reader.read_exact(&mut bytes).map_err(frame_io_error)?;
    Ok(u64::from_be_bytes(bytes))
}

fn frame_io_error(error: io::Error) -> LinkError {
    let kind = if matches!(
        error.kind(),
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
    ) {
        ErrorKind::Timeout
    } else {
        ErrorKind::DaemonUnavailable
    };
    LinkError::new(kind, "daemon IPC transport failed").with_detail("reason", error.to_string())
}

fn ipc_io_error(message: &'static str, socket: &Path, error: &io::Error) -> LinkError {
    LinkError::new(ErrorKind::DaemonUnavailable, message)
        .with_detail("socket", socket.display().to_string())
        .with_detail("reason", error.to_string())
}

fn next_request_id() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// Remove a stale socket only when it is a socket owned by the current user.
pub fn remove_stale_socket(path: &Path) -> Result<(), LinkError> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(ipc_io_error(
                "failed to inspect daemon socket",
                path,
                &error,
            ));
        }
    };
    if !metadata.file_type().is_socket() || metadata.uid() != rustix::process::getuid().as_raw() {
        return Err(LinkError::new(
            ErrorKind::PermissionDenied,
            "refusing to replace an unsafe daemon socket path",
        )
        .with_detail("socket", path.display().to_string()));
    }
    fs::remove_file(path)
        .map_err(|error| ipc_io_error("failed to remove stale socket", path, &error))
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    #[test]
    fn message_round_trip_preserves_binary_payload() {
        let request = RequestEnvelope::new(
            7,
            Operation::Snapshot {
                encoding: SnapshotEncoding::Jpeg,
            },
        );
        let mut bytes = Vec::new();
        write_message(&mut bytes, &request, b"jpeg").unwrap();
        let (decoded, binary): (RequestEnvelope, Vec<u8>) =
            read_message(&mut Cursor::new(bytes)).unwrap();
        assert_eq!(decoded.request_id, 7);
        assert_eq!(binary, b"jpeg");
    }

    #[test]
    fn rejects_incompatible_protocol() {
        let error = validate_protocol(PROTOCOL_VERSION + 1).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::DaemonUnavailable);
        assert_eq!(error.process_exit().code(), 12);
    }

    #[test]
    fn virtual_camera_defaults_are_explicit() {
        let spec = VirtualCameraSpec::default();
        assert_eq!(spec.name, "clean");
        assert_eq!(spec.output_device, Path::new("/dev/video20"));
        assert_eq!(spec.width, 1920);
        assert_eq!(spec.height, 1080);
    }

    #[test]
    fn control_transaction_round_trip_preserves_policy() {
        let request = RequestEnvelope::new(
            11,
            Operation::ControlSet {
                writes: vec![StandardControlWrite {
                    selector: "brightness".into(),
                    value: "52%".into(),
                }],
                raw: false,
                clamp: true,
                batched: true,
                fallback_individual: true,
                dry_run: true,
            },
        );
        let mut bytes = Vec::new();
        write_message(&mut bytes, &request, &[]).unwrap();
        let (decoded, _): (RequestEnvelope, Vec<u8>) =
            read_message(&mut Cursor::new(bytes)).unwrap();
        let Operation::ControlSet {
            writes,
            raw,
            clamp,
            batched,
            fallback_individual,
            dry_run,
        } = decoded.operation
        else {
            panic!("unexpected operation");
        };
        assert_eq!(writes[0].value, "52%");
        assert!(!raw);
        assert!(clamp);
        assert!(batched);
        assert!(fallback_individual);
        assert!(dry_run);
    }
}
