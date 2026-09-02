//! A private system-bus player for Bluetooth lyrics. The session-bus MPRIS
//! player keeps real song metadata for Plasma and other desktop clients.
use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result};
use async_channel::{Receiver, Sender};
use tokio::sync::watch;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

use crate::mpris::MprisCommand;

const PATH: &str = "/dev/lyrune/BluetoothLyrics";
const PLAYER: &str = "org.mpris.MediaPlayer2.Player";
const TIMEOUT: Duration = Duration::from_secs(3);
const RETRY: Duration = Duration::from_secs(2);
const TEST_DURATION: Duration = Duration::from_secs(5);
type Properties = HashMap<String, OwnedValue>;
type Objects = HashMap<OwnedObjectPath, HashMap<String, HashMap<String, OwnedValue>>>;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Snapshot {
    pub track_id: String,
    pub title: String,
    pub artist: String,
    pub length_micros: i64,
    pub position_micros: i64,
    pub playing: bool,
    pub stopped: bool,
}

impl Snapshot {
    fn status(&self) -> &'static str {
        if self.playing {
            "Playing"
        } else if self.stopped {
            "Stopped"
        } else {
            "Paused"
        }
    }

    fn metadata(&self) -> Properties {
        let mut props = Properties::new();
        let track_path = if self.track_id.is_empty() {
            "/org/mpris/MediaPlayer2/TrackList/NoTrack".to_owned()
        } else {
            let encoded: String = self
                .track_id
                .bytes()
                .map(|byte| format!("{byte:02x}"))
                .collect();
            format!("{PATH}/track_{encoded}")
        };
        props.insert(
            "mpris:trackid".into(),
            owned(OwnedObjectPath::try_from(track_path).expect("hex track path")),
        );
        props.insert("xesam:title".into(), owned(normalize_text(&self.title)));
        props.insert(
            "xesam:artist".into(),
            owned(vec![normalize_text(&self.artist)]),
        );
        props.insert("xesam:album".into(), owned("Lyrune 蓝牙歌词"));
        props.insert("mpris:length".into(), self.length_micros.max(0).into());
        props
    }

    fn properties(&self) -> Properties {
        let mut props = Properties::new();
        props.insert("Identity".into(), owned("Lyrune · PixelBar Lyrics"));
        props.insert("Metadata".into(), owned(self.metadata()));
        props.insert("PlaybackStatus".into(), owned(self.status()));
        props.insert("Position".into(), self.position_micros.max(0).into());
        for name in [
            "CanPlay",
            "CanPause",
            "CanGoNext",
            "CanGoPrevious",
            "CanControl",
        ] {
            props.insert(name.into(), (!self.track_id.is_empty()).into());
        }
        props
    }

    fn same_metadata(&self, other: &Self) -> bool {
        self.track_id == other.track_id
            && self.title == other.title
            && self.artist == other.artist
            && self.length_micros == other.length_micros
    }
}

fn owned<'a>(value: impl Into<Value<'a>>) -> OwnedValue {
    value.into().try_into().expect("owned D-Bus value")
}

pub fn normalize_text(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .filter(|c| !c.is_control())
        .take(120)
        .collect()
}

/// Positive offsets delay lyrics; negative offsets advance them.
pub fn lyric_position(position: Duration, offset_ms: i32) -> Duration {
    let offset = offset_ms.clamp(-3000, 3000);
    if offset >= 0 {
        position.saturating_sub(Duration::from_millis(offset as u64))
    } else {
        position.saturating_add(Duration::from_millis(offset.unsigned_abs() as u64))
    }
}

#[derive(Clone, Default)]
struct Request {
    enabled: bool,
    snapshot: Snapshot,
    test_serial: u64,
}

pub enum Event {
    Status(String),
    Command(MprisCommand),
}

pub struct BluetoothLyrics {
    updates: Option<watch::Sender<Request>>,
    thread: Option<thread::JoinHandle<()>>,
}

impl BluetoothLyrics {
    pub fn start() -> Result<(Self, Receiver<Event>)> {
        let (updates, receiver) = watch::channel(Request::default());
        let (events, event_receiver) = async_channel::unbounded();
        let worker = thread::Builder::new()
            .name("lyrune-bluetooth-lyrics".into())
            .spawn(move || {
                match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime.block_on(run(receiver, events)),
                    Err(error) => {
                        let _ =
                            events.try_send(Event::Status(format!("蓝牙歌词启动失败：{error}")));
                    }
                }
            })
            .context("无法启动蓝牙歌词线程")?;
        Ok((
            Self {
                updates: Some(updates),
                thread: Some(worker),
            },
            event_receiver,
        ))
    }

    pub fn update(&self, enabled: bool, snapshot: Snapshot) {
        if let Some(sender) = &self.updates {
            sender.send_if_modified(|request| {
                if request.enabled == enabled && request.snapshot == snapshot {
                    return false;
                }
                request.enabled = enabled;
                request.snapshot = snapshot;
                true
            });
        }
    }

    pub fn test(&self) {
        if let Some(sender) = &self.updates {
            sender.send_modify(|request| request.test_serial = request.test_serial.wrapping_add(1));
        }
    }
}

impl Drop for BluetoothLyrics {
    fn drop(&mut self) {
        self.updates.take();
        if let Some(worker) = self.thread.take() {
            let _ = worker.join();
        }
    }
}

struct Player {
    state: Arc<RwLock<Snapshot>>,
    events: Sender<Event>,
}

impl Player {
    fn command(&self, command: MprisCommand) {
        let _ = self.events.try_send(Event::Command(command));
    }
    fn snapshot(&self) -> Snapshot {
        self.state.read().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

#[zbus::interface(name = "org.mpris.MediaPlayer2.Player")]
impl Player {
    fn play(&self) {
        self.command(MprisCommand::Play);
    }
    fn pause(&self) {
        self.command(MprisCommand::Pause);
    }
    fn play_pause(&self) {
        self.command(MprisCommand::PlayPause);
    }
    fn stop(&self) {
        self.command(MprisCommand::Stop);
    }
    fn next(&self) {
        self.command(MprisCommand::Next);
    }
    fn previous(&self) {
        self.command(MprisCommand::Previous);
    }
    #[zbus(property)]
    fn playback_status(&self) -> String {
        self.snapshot().status().into()
    }
    #[zbus(property)]
    fn metadata(&self) -> Properties {
        self.snapshot().metadata()
    }
    #[zbus(property)]
    fn position(&self) -> i64 {
        self.snapshot().position_micros
    }
    #[zbus(property)]
    fn can_control(&self) -> bool {
        !self.snapshot().track_id.is_empty()
    }
    #[zbus(property)]
    fn can_play(&self) -> bool {
        self.can_control()
    }
    #[zbus(property)]
    fn can_pause(&self) -> bool {
        self.can_control()
    }
    #[zbus(property)]
    fn can_go_next(&self) -> bool {
        self.can_control()
    }
    #[zbus(property)]
    fn can_go_previous(&self) -> bool {
        self.can_control()
    }
    #[zbus(property)]
    fn can_seek(&self) -> bool {
        false
    }
    #[zbus(property)]
    fn rate(&self) -> f64 {
        1.0
    }
    #[zbus(property)]
    fn minimum_rate(&self) -> f64 {
        1.0
    }
    #[zbus(property)]
    fn maximum_rate(&self) -> f64 {
        1.0
    }
}

fn pixelbar_adapters(objects: &Objects) -> BTreeSet<String> {
    let mut adapters = BTreeSet::new();
    for interfaces in objects.values() {
        let Some(device) = interfaces.get("org.bluez.Device1") else {
            continue;
        };
        if !device
            .get("Connected")
            .and_then(|v| bool::try_from(v).ok())
            .unwrap_or(false)
        {
            continue;
        }
        let matches = ["Name", "Alias"].iter().any(|key| {
            device
                .get(*key)
                .and_then(|v| <&str>::try_from(v).ok())
                .is_some_and(|name| {
                    name.to_lowercase()
                        .replace([' ', '-', '_'], "")
                        .contains("pixelbar")
                })
        });
        if !matches {
            continue;
        }
        let Some(adapter) = device
            .get("Adapter")
            .and_then(|v| <&zbus::zvariant::ObjectPath<'_>>::try_from(v).ok())
        else {
            continue;
        };
        if objects
            .get(adapter)
            .is_some_and(|interfaces| interfaces.contains_key("org.bluez.Media1"))
        {
            adapters.insert(adapter.to_string());
        }
    }
    adapters
}

struct Bridge {
    connection: zbus::Connection,
    state: Arc<RwLock<Snapshot>>,
    owner: String,
    adapters: BTreeSet<String>,
    published: Option<Snapshot>,
}

impl Bridge {
    async fn connect(snapshot: Snapshot, events: Sender<Event>) -> Result<Self> {
        let state = Arc::new(RwLock::new(snapshot));
        let connection = zbus::connection::Builder::system()?
            .serve_at(
                PATH,
                Player {
                    state: state.clone(),
                    events,
                },
            )?
            .build()
            .await?;
        Ok(Self {
            connection,
            state,
            owner: String::new(),
            adapters: BTreeSet::new(),
            published: None,
        })
    }

    async fn reconcile(&mut self, snapshot: &Snapshot) -> Result<bool> {
        let bus = zbus::Proxy::new(
            &self.connection,
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
        )
        .await?;
        let owner: String = bus.call("GetNameOwner", &("org.bluez",)).await?;
        if self.owner != owner {
            self.owner = owner;
            self.adapters.clear();
            self.published = None;
        }
        let manager = zbus::Proxy::new(
            &self.connection,
            "org.bluez",
            "/",
            "org.freedesktop.DBus.ObjectManager",
        )
        .await?;
        let objects: Objects = manager.call("GetManagedObjects", &()).await?;
        let desired = pixelbar_adapters(&objects);
        // Unregister on disconnect. Dropping the connection also unregisters
        // everything on disable/exit/error, including partially completed calls.
        for adapter in self.adapters.difference(&desired) {
            let media = zbus::Proxy::new(
                &self.connection,
                "org.bluez",
                adapter.as_str(),
                "org.bluez.Media1",
            )
            .await?;
            let _: () = media
                .call(
                    "UnregisterPlayer",
                    &(zbus::zvariant::ObjectPath::try_from(PATH)?,),
                )
                .await?;
        }
        *self.state.write().unwrap_or_else(|e| e.into_inner()) = snapshot.clone();
        for adapter in desired.difference(&self.adapters) {
            let media = zbus::Proxy::new(
                &self.connection,
                "org.bluez",
                adapter.as_str(),
                "org.bluez.Media1",
            )
            .await?;
            let _: () = media
                .call(
                    "RegisterPlayer",
                    &(
                        zbus::zvariant::ObjectPath::try_from(PATH)?,
                        snapshot.properties(),
                    ),
                )
                .await?;
            self.published = None;
        }
        self.adapters = desired;
        Ok(!self.adapters.is_empty())
    }

    async fn publish(&mut self, snapshot: &Snapshot) -> Result<()> {
        *self.state.write().unwrap_or_else(|e| e.into_inner()) = snapshot.clone();
        if self.adapters.is_empty() {
            return Ok(());
        }
        let changed = self
            .published
            .as_ref()
            .is_none_or(|old| !old.same_metadata(snapshot));
        let status_changed = self
            .published
            .as_ref()
            .is_none_or(|old| old.status() != snapshot.status());
        if changed || status_changed {
            let mut props = snapshot.properties();
            props.remove("Identity");
            // BlueZ resets its position when Metadata changes. Send the
            // position separately afterwards so dictionary order cannot race it.
            props.remove("Position");
            if !changed {
                props.remove("Metadata");
            }
            self.signal(props).await?;
        }
        let mut position = Properties::new();
        position.insert("Position".into(), snapshot.position_micros.max(0).into());
        self.signal(position).await?;
        self.published = Some(snapshot.clone());
        Ok(())
    }

    async fn signal(&self, props: Properties) -> Result<()> {
        self.connection
            .emit_signal(
                None::<&str>,
                PATH,
                "org.freedesktop.DBus.Properties",
                "PropertiesChanged",
                &(PLAYER, props, Vec::<String>::new()),
            )
            .await?;
        Ok(())
    }
}

async fn run(mut receiver: watch::Receiver<Request>, events: Sender<Event>) {
    let mut bridge: Option<Bridge> = None;
    let mut last_status = String::new();
    let mut next_check = Instant::now();
    let mut test_serial = 0;
    let mut test_until: Option<Instant> = None;
    let mut interval = tokio::time::interval(RETRY);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            result = receiver.changed() => { if result.is_err() { break; } }
            _ = interval.tick() => {}
        }
        let request = receiver.borrow_and_update().clone();
        let status = if !request.enabled {
            bridge = None;
            test_until = None;
            test_serial = request.test_serial;
            next_check = Instant::now();
            "蓝牙歌词已关闭".to_owned()
        } else {
            if request.test_serial != test_serial {
                test_serial = request.test_serial;
                test_until = Some(Instant::now() + TEST_DURATION);
            }
            let mut snapshot = request.snapshot;
            let testing = test_until.is_some_and(|until| Instant::now() < until);
            if testing {
                snapshot.title = "PixelBar 蓝牙歌词测试".into();
                snapshot.playing = true;
                snapshot.stopped = false;
            }
            if bridge.is_none() && Instant::now() < next_check {
                continue;
            }
            let result = tokio::time::timeout(TIMEOUT, async {
                if bridge.is_none() {
                    bridge = Some(Bridge::connect(snapshot.clone(), events.clone()).await?);
                    next_check = Instant::now();
                }
                let bridge = bridge.as_mut().expect("connected above");
                if Instant::now() >= next_check {
                    bridge.reconcile(&snapshot).await?;
                    next_check = Instant::now() + RETRY;
                }
                bridge.publish(&snapshot).await?;
                Ok::<_, anyhow::Error>(!bridge.adapters.is_empty())
            })
            .await;
            match result {
                Ok(Ok(true)) => if testing {
                    "测试文字已发布，请查看音响屏幕"
                } else {
                    "PixelBar 已连接，蓝牙歌词输出中"
                }
                .into(),
                Ok(Ok(false)) => "等待连接 PixelBar，请选择蓝牙输入并开启音乐可视化".into(),
                error => {
                    bridge = None;
                    next_check = Instant::now() + RETRY;
                    match error {
                        Ok(Err(error)) => format!("蓝牙歌词暂不可用，将自动重试：{error:#}"),
                        Err(_) => "蓝牙服务响应超时，将自动重试".into(),
                        _ => unreachable!(),
                    }
                }
            }
        };
        if status != last_status {
            last_status = status.clone();
            if events.try_send(Event::Status(status)).is_err() {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unicode_text_and_offsets_are_safe() {
        assert_eq!(normalize_text("  你好\n世界\t "), "你好 世界");
        assert_eq!(normalize_text(&"你".repeat(200)).chars().count(), 120);
        assert_eq!(
            lyric_position(Duration::from_millis(100), 500),
            Duration::ZERO
        );
        assert_eq!(
            lyric_position(Duration::from_secs(2), -500),
            Duration::from_millis(2500)
        );
        assert_eq!(
            lyric_position(Duration::from_secs(5), 9999),
            Duration::from_secs(2)
        );
    }

    #[test]
    fn metadata_has_valid_track_paths_and_nested_dictionary() {
        let snapshot = Snapshot {
            track_id: "歌/曲 #1".into(),
            title: "你好".into(),
            ..Default::default()
        };
        let props = snapshot.properties();
        let metadata = <HashMap<String, OwnedValue>>::try_from(
            props.get("Metadata").unwrap().try_clone().unwrap(),
        )
        .unwrap();
        assert_eq!(
            <&str>::try_from(metadata.get("xesam:title").unwrap()).unwrap(),
            "你好"
        );
        let path =
            <&zbus::zvariant::ObjectPath<'_>>::try_from(metadata.get("mpris:trackid").unwrap())
                .unwrap();
        assert!(path.as_str().starts_with(PATH));
        assert!(!path.as_str().contains(' '));
    }

    #[test]
    fn progress_does_not_change_metadata_but_track_switch_does() {
        let original = Snapshot {
            track_id: "one".into(),
            title: "重复歌词".into(),
            ..Default::default()
        };
        let mut next = original.clone();
        next.position_micros = 1_000_000;
        assert!(original.same_metadata(&next));
        next.track_id = "two".into();
        assert!(!original.same_metadata(&next));
    }

    #[test]
    fn only_connected_pixelbar_adapters_are_registered() {
        let adapter = OwnedObjectPath::try_from("/org/bluez/hci0").unwrap();
        let mut objects = Objects::new();
        objects.insert(
            adapter.clone(),
            HashMap::from([("org.bluez.Media1".into(), HashMap::new())]),
        );
        let device = OwnedObjectPath::try_from("/org/bluez/hci0/dev_AA_BB").unwrap();
        let props = HashMap::from([
            ("Name".into(), owned("花再 Halo PixelBar")),
            ("Connected".into(), true.into()),
            ("Adapter".into(), owned(adapter)),
        ]);
        objects.insert(
            device.clone(),
            HashMap::from([("org.bluez.Device1".into(), props)]),
        );
        assert_eq!(
            pixelbar_adapters(&objects),
            BTreeSet::from(["/org/bluez/hci0".into()])
        );
        objects
            .get_mut(&device)
            .unwrap()
            .get_mut("org.bluez.Device1")
            .unwrap()
            .insert("Connected".into(), false.into());
        assert!(pixelbar_adapters(&objects).is_empty());
    }
}
