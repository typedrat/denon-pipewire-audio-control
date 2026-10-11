//! Wire types for `org.pipewire.ExternalVolume`.

use serde::{Deserialize, Serialize};
use zlink::introspect;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, introspect::Type)]
pub struct VolumeRange {
    pub min: f64,
    pub max: f64,
    pub step: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, introspect::Type)]
pub struct VolumeStepRange {
    pub min: f64,
    pub max: f64,
}

/// Sent as the reply parameters themselves rather than under a `capabilities` key: PipeWire's
/// client reads the flags straight out of `parameters`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, introspect::Type)]
#[serde(rename_all = "camelCase")]
pub struct Capabilities {
    pub read_volume: bool,
    pub read_balance: bool,
    pub volume_range: VolumeRange,
    pub write_volume_absolute: bool,
    pub write_volume_relative: bool,
    pub write_volume_relative_step: VolumeStepRange,
    pub write_balance: bool,
    pub read_mute: bool,
    pub write_mute_value: bool,
    pub write_mute_toggle: bool,
    pub routes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, introspect::Type)]
pub struct Volume {
    pub volume: Vec<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, introspect::Type)]
pub struct Mute {
    pub mute: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, introspect::Type)]
pub struct MonitorUpdate {
    pub route: String,
    pub volume: Vec<f64>,
    pub mute: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, zlink::ReplyError, introspect::ReplyError)]
#[zlink(interface = "org.pipewire.ExternalVolume")]
pub enum ExternalVolumeError {
    InvalidDevice,
    InvalidRoute,
    OperationUnsupported,
}
