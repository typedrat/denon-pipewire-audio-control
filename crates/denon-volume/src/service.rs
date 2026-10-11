//! The `org.pipewire.ExternalVolume` Varlink service.

use std::future;
use std::sync::Arc;
use std::time::Duration;

use futures_util::{Stream, StreamExt};
use tokio_stream::wrappers::WatchStream;
use tracing::{debug, warn};
use zlink::{Reply, service};

use crate::avr::Outbox;
use crate::protocol::Command;
use crate::scale;
use crate::state::Shared;
use crate::types::{
    Capabilities, ExternalVolumeError, MonitorUpdate, Mute, Volume, VolumeRange, VolumeStepRange,
};

/// PipeWire reads the volume right after asking for capabilities, and gives that first call
/// 2 s. Waiting a little for the receiver to report in means that read gets a real answer.
const FIRST_STATE_WAIT: Duration = Duration::from_millis(1500);

pub struct ExternalVolume {
    device: String,
    route: String,
    shared: Arc<Shared>,
    outbox: Arc<Outbox>,
}

impl ExternalVolume {
    pub fn new(
        device: impl Into<String>,
        route: impl Into<String>,
        shared: Arc<Shared>,
        outbox: Arc<Outbox>,
    ) -> Self {
        Self {
            device: device.into(),
            route: route.into(),
            shared,
            outbox,
        }
    }

    fn check_device(&self, device: &str) -> Result<(), ExternalVolumeError> {
        if device == self.device {
            Ok(())
        } else {
            warn!(device, expected = %self.device, "call for an unknown device");
            Err(ExternalVolumeError::InvalidDevice)
        }
    }

    fn check(&self, device: &str, route: &str) -> Result<(), ExternalVolumeError> {
        self.check_device(device)?;
        if route == self.route {
            Ok(())
        } else {
            warn!(route, expected = %self.route, "call for an unknown route");
            Err(ExternalVolumeError::InvalidRoute)
        }
    }

    fn write_mute(&self, mute: bool) {
        if self.shared.request_mute() {
            self.outbox.push(Command::Mute(mute));
        } else {
            debug!(mute, "receiver isn't on the PC input; ignoring mute write");
        }
    }
}

#[service(
    interface = "org.pipewire.ExternalVolume",
    product = "denon-volume",
    version = env!("CARGO_PKG_VERSION")
)]
impl ExternalVolume {
    #[zlink(rename = "GetCapabilities")]
    async fn get_capabilities(&self, device: &str) -> Result<Capabilities, ExternalVolumeError> {
        self.check_device(device)?;
        let mut rx = self.shared.subscribe();
        let known = rx.wait_for(|s| s.mv.is_some() && s.mute.is_some());
        if tokio::time::timeout(FIRST_STATE_WAIT, known).await.is_err() {
            warn!("receiver state still unknown; PipeWire's first read will fail");
        }
        Ok(Capabilities {
            read_volume: true,
            read_balance: false,
            volume_range: VolumeRange {
                min: 0.0,
                max: scale::from_mv(self.shared.cap()),
                step: 0.005,
            },
            write_volume_absolute: true,
            write_volume_relative: false,
            write_volume_relative_step: VolumeStepRange { min: 0.0, max: 0.0 },
            write_balance: false,
            read_mute: true,
            write_mute_value: true,
            write_mute_toggle: true,
            routes: vec![self.route.clone()],
        })
    }

    #[zlink(rename = "ReadVolume")]
    async fn read_volume(&self, device: &str, route: &str) -> Result<Volume, ExternalVolumeError> {
        self.check(device, route)?;
        let volume = self
            .shared
            .snapshot()
            .volume()
            .ok_or(ExternalVolumeError::OperationUnsupported)?;
        Ok(Volume {
            volume: vec![volume],
        })
    }

    #[zlink(rename = "ReadMute")]
    async fn read_mute(&self, device: &str, route: &str) -> Result<Mute, ExternalVolumeError> {
        self.check(device, route)?;
        let mute = self
            .shared
            .snapshot()
            .mute
            .ok_or(ExternalVolumeError::OperationUnsupported)?;
        Ok(Mute { mute })
    }

    #[zlink(rename = "WriteVolumeAbsolute")]
    async fn write_volume_absolute(
        &self,
        device: &str,
        route: &str,
        volume: Vec<f64>,
    ) -> Result<(), ExternalVolumeError> {
        self.check(device, route)?;
        let Some(v) = volume.into_iter().reduce(f64::max) else {
            return Ok(());
        };
        match self.shared.request_volume(v) {
            Some(mv) => self.outbox.push(Command::Volume(mv)),
            None => debug!(
                volume = v,
                "receiver isn't on the PC input; ignoring volume write"
            ),
        }
        Ok(())
    }

    #[zlink(rename = "WriteVolumeRelative")]
    async fn write_volume_relative(
        &self,
        device: &str,
        route: &str,
        step: f64,
    ) -> Result<(), ExternalVolumeError> {
        self.check(device, route)?;
        debug!(step, "relative volume isn't advertised");
        Err(ExternalVolumeError::OperationUnsupported)
    }

    #[zlink(rename = "WriteMuteValue")]
    async fn write_mute_value(
        &self,
        device: &str,
        route: &str,
        mute: bool,
    ) -> Result<(), ExternalVolumeError> {
        self.check(device, route)?;
        self.write_mute(mute);
        Ok(())
    }

    #[zlink(rename = "WriteMuteToggle")]
    async fn write_mute_toggle(
        &self,
        device: &str,
        route: &str,
    ) -> Result<(), ExternalVolumeError> {
        self.check(device, route)?;
        let muted = self.shared.snapshot().mute.unwrap_or(false);
        self.write_mute(!muted);
        Ok(())
    }

    #[zlink(rename = "Monitor", more)]
    async fn monitor(
        &self,
        more: bool,
        device: String,
    ) -> impl Stream<Item = Reply<MonitorUpdate>> + use<> {
        if device != self.device {
            warn!(device, expected = %self.device, "monitor for an unknown device");
        }
        let route = self.route.clone();
        WatchStream::new(self.shared.subscribe())
            .filter(|s| future::ready(s.mv.is_some()))
            .take(if more { usize::MAX } else { 1 })
            .map(move |s| {
                let update = MonitorUpdate {
                    route: route.clone(),
                    volume: s.volume().into_iter().collect(),
                    mute: s.mute,
                };
                Reply::new(Some(update)).set_continues(more.then_some(true))
            })
    }
}
