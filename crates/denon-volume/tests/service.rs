use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use denon_volume::avr::Outbox;
use denon_volume::protocol::{Command, Event};
use denon_volume::scale::Mv;
use denon_volume::service::ExternalVolume;
use denon_volume::state::Shared;
use denon_volume::types::{Capabilities, ExternalVolumeError, MonitorUpdate, Mute, Volume};
use futures_util::{Stream, StreamExt, pin_mut};
use tokio::time;
use zlink::unix;

const DEVICE: &str = "alsa_card.test";
const ROUTE: &str = "hdmi-output-0";
const CAP: Mv = Mv::from_whole(70);

#[zlink::proxy("org.pipewire.ExternalVolume")]
trait ExternalVolumeProxy {
    async fn get_capabilities(
        &mut self,
        device: &str,
    ) -> zlink::Result<Result<Capabilities, ExternalVolumeError>>;
    async fn read_volume(
        &mut self,
        device: &str,
        route: &str,
    ) -> zlink::Result<Result<Volume, ExternalVolumeError>>;
    async fn read_mute(
        &mut self,
        device: &str,
        route: &str,
    ) -> zlink::Result<Result<Mute, ExternalVolumeError>>;
    async fn write_volume_absolute(
        &mut self,
        device: &str,
        route: &str,
        volume: &[f64],
    ) -> zlink::Result<Result<(), ExternalVolumeError>>;
    async fn write_mute_value(
        &mut self,
        device: &str,
        route: &str,
        mute: bool,
    ) -> zlink::Result<Result<(), ExternalVolumeError>>;
    #[zlink(more)]
    async fn monitor(
        &mut self,
        device: &str,
    ) -> zlink::Result<impl Stream<Item = zlink::Result<Result<MonitorUpdate, ExternalVolumeError>>>>;
}

struct Harness {
    shared: Arc<Shared>,
    outbox: Arc<Outbox>,
    path: PathBuf,
}

impl Harness {
    fn receiver_on_pc(&self) {
        self.shared.set_connected(true);
        self.shared.apply(Event::Power(true));
        self.shared.apply(Event::Input("BD".into()));
        self.shared.apply(Event::Volume(Mv::from_whole(62)));
        self.shared.apply(Event::Mute(false));
    }
}

async fn with_service<F, Fut>(body: F)
where
    F: FnOnce(Harness) -> Fut,
    Fut: Future<Output = ()>,
{
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ev.sock");
    let shared = Arc::new(Shared::new("BD", CAP));
    let outbox = Arc::new(Outbox::default());
    let service = ExternalVolume::new(DEVICE, ROUTE, shared.clone(), outbox.clone());
    let server = zlink::Server::new(unix::bind(&path).unwrap(), service);
    let harness = Harness {
        shared,
        outbox,
        path,
    };
    tokio::select! {
        result = server.run() => panic!("server exited: {result:?}"),
        () = body(harness) => {}
    }
}

async fn next_update(
    stream: &mut (impl Stream<Item = zlink::Result<Result<MonitorUpdate, ExternalVolumeError>>> + Unpin),
) -> MonitorUpdate {
    time::timeout(Duration::from_secs(2), stream.next())
        .await
        .expect("no monitor update")
        .expect("stream ended")
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn advertises_absolute_volume_and_mute() {
    with_service(|h| async move {
        h.receiver_on_pc();
        let mut conn = unix::connect(&h.path).await.unwrap();
        let caps = conn.get_capabilities(DEVICE).await.unwrap().unwrap();
        assert!(caps.read_volume && caps.write_volume_absolute);
        assert!(caps.read_mute && caps.write_mute_value);
        assert!(!caps.write_volume_relative);
        assert_eq!(caps.routes, [ROUTE]);
        assert_eq!(
            conn.get_capabilities("other").await.unwrap(),
            Err(ExternalVolumeError::InvalidDevice)
        );
    })
    .await;
}

#[tokio::test]
async fn reads_need_a_known_state() {
    with_service(|h| async move {
        let mut conn = unix::connect(&h.path).await.unwrap();
        assert_eq!(
            conn.read_volume(DEVICE, ROUTE).await.unwrap(),
            Err(ExternalVolumeError::OperationUnsupported)
        );
        h.receiver_on_pc();
        let volume = conn.read_volume(DEVICE, ROUTE).await.unwrap().unwrap();
        assert_eq!(volume.volume, [0.62]);
        assert!(!conn.read_mute(DEVICE, ROUTE).await.unwrap().unwrap().mute);
        assert_eq!(
            conn.read_volume(DEVICE, "hdmi-output-1").await.unwrap(),
            Err(ExternalVolumeError::InvalidRoute)
        );
    })
    .await;
}

#[tokio::test]
async fn writes_on_the_pc_input_reach_the_outbox() {
    with_service(|h| async move {
        h.receiver_on_pc();
        let mut conn = unix::connect(&h.path).await.unwrap();
        conn.write_volume_absolute(DEVICE, ROUTE, &[0.613, 0.613])
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            h.outbox.take(),
            Some(Command::Volume(Mv::from_half_steps(123)))
        );
        conn.write_mute_value(DEVICE, ROUTE, true)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(h.outbox.take(), Some(Command::Mute(true)));

        h.shared.apply(Event::Volume(Mv::from_half_steps(123)));
        let volume = conn.read_volume(DEVICE, ROUTE).await.unwrap().unwrap();
        assert_eq!(volume.volume, [0.613]);

        conn.write_volume_absolute(DEVICE, ROUTE, &[0.9])
            .await
            .unwrap()
            .unwrap();
        assert_eq!(h.outbox.take(), Some(Command::Volume(CAP)));
    })
    .await;
}

#[tokio::test]
async fn writes_elsewhere_are_dropped_and_snap_back() {
    with_service(|h| async move {
        h.receiver_on_pc();
        h.shared.apply(Event::Input("GAME".into()));
        let mut monitor_conn = unix::connect(&h.path).await.unwrap();
        let mut conn = unix::connect(&h.path).await.unwrap();
        let stream = monitor_conn.monitor(DEVICE).await.unwrap();
        pin_mut!(stream);
        let real = next_update(&mut stream).await;

        conn.write_volume_absolute(DEVICE, ROUTE, &[1.0])
            .await
            .unwrap()
            .unwrap();
        assert_eq!(h.outbox.take(), None);
        assert_eq!(next_update(&mut stream).await, real);
    })
    .await;
}

#[tokio::test]
async fn monitor_streams_receiver_changes() {
    with_service(|h| async move {
        let mut conn = unix::connect(&h.path).await.unwrap();
        let stream = conn.monitor(DEVICE).await.unwrap();
        pin_mut!(stream);

        h.receiver_on_pc();
        let first = next_update(&mut stream).await;
        assert_eq!(first.route, ROUTE);
        assert_eq!(first.volume, [0.62]);

        h.shared.apply(Event::Volume(Mv::from_whole(60)));
        let update = next_update(&mut stream).await;
        assert_eq!(update.volume, [0.60]);

        h.shared.apply(Event::Mute(true));
        assert_eq!(next_update(&mut stream).await.mute, Some(true));
    })
    .await;
}

#[tokio::test]
async fn writes_right_after_get_capabilities_are_refused_and_snap_back() {
    with_service(|h| async move {
        h.receiver_on_pc();
        let mut monitor_conn = unix::connect(&h.path).await.unwrap();
        let mut conn = unix::connect(&h.path).await.unwrap();
        let stream = monitor_conn.monitor(DEVICE).await.unwrap();
        pin_mut!(stream);
        let real = next_update(&mut stream).await;

        conn.get_capabilities(DEVICE).await.unwrap().unwrap();
        conn.write_volume_absolute(DEVICE, ROUTE, &[0.2])
            .await
            .unwrap()
            .unwrap();
        conn.write_mute_value(DEVICE, ROUTE, true)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(h.outbox.take(), None);
        assert_eq!(next_update(&mut stream).await, real);
    })
    .await;
}
