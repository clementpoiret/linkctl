//! Versioned machine-readable output envelopes.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::{LinkError, SCHEMA_VERSION};

/// Non-secret device identity included in command output when available.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DeviceSummary {
    /// Redacted stable identifier.
    pub stable_id: String,
    /// Human-readable model name.
    pub model: String,
}

/// Structured error body embedded in an [`Envelope`].
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ErrorBody {
    /// Stable kebab-case error code.
    pub code: String,
    /// Process exit code.
    pub exit_code: u8,
    /// Human-readable error message.
    pub message: String,
    /// Machine-readable context.
    pub details: Map<String, Value>,
}

impl From<&LinkError> for ErrorBody {
    fn from(error: &LinkError) -> Self {
        Self {
            code: error.kind().code().to_owned(),
            exit_code: error.process_exit().code(),
            message: error.message().to_owned(),
            details: error.details().clone(),
        }
    }
}

/// Stable JSON/JSON Lines response envelope.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Envelope<T> {
    /// Machine schema version.
    pub schema_version: u32,
    /// Whether the command succeeded.
    pub ok: bool,
    /// Stable command identifier.
    pub command: String,
    /// Selected device, or `null` before selection.
    pub device: Option<DeviceSummary>,
    /// Command result, or `null` on error.
    pub result: Option<T>,
    /// Error body, or `null` on success.
    pub error: Option<ErrorBody>,
}

impl<T> Envelope<T> {
    /// Construct a successful response.
    #[must_use]
    pub fn success(command: impl Into<String>, device: Option<DeviceSummary>, result: T) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            ok: true,
            command: command.into(),
            device,
            result: Some(result),
            error: None,
        }
    }

    /// Construct an error response.
    #[must_use]
    pub fn failure(
        command: impl Into<String>,
        device: Option<DeviceSummary>,
        error: &LinkError,
    ) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            ok: false,
            command: command.into(),
            device,
            result: None,
            error: Some(ErrorBody::from(error)),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use crate::{ErrorKind, LinkError, output::Envelope};

    #[test]
    fn success_envelope_keeps_all_top_level_fields() {
        assert_eq!(crate::SCHEMA_VERSION, 1);
        let value = serde_json::to_value(Envelope::success("test", None, json!({"value": 1})))
            .expect("envelope should serialize");

        assert_eq!(
            value,
            json!({
                "schema_version": 1,
                "ok": true,
                "command": "test",
                "device": null,
                "result": {"value": 1},
                "error": null
            })
        );
    }

    #[test]
    fn error_envelope_keeps_all_top_level_fields() {
        let error = LinkError::new(ErrorKind::DeviceBusy, "busy").with_detail("owner", "obs");
        let envelope: Envelope<Value> = Envelope::failure("test", None, &error);
        let value = serde_json::to_value(envelope).expect("envelope should serialize");

        assert_eq!(value["schema_version"], 1);
        assert_eq!(value["ok"], false);
        assert!(value["result"].is_null());
        assert_eq!(value["error"]["code"], "device-busy");
        assert_eq!(value["error"]["exit_code"], 5);
        assert_eq!(value["error"]["details"]["owner"], "obs");
    }
}
