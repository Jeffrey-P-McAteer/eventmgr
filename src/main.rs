
#![allow(unused_imports, dead_code, unused_variables)]

use futures::prelude::*;

use std::borrow::Borrow;
use std::str::FromStr;
use measure_time::{info_time, debug_time, trace_time, error_time, print_time};
use tokio::time::MissedTickBehavior;

pub const SERVER_SOCKET: &'static str = "/tmp/eventmgr.sock";

pub mod structs;
#[macro_use]
pub mod macros;

pub mod client;

pub mod util;

use structs::*;
use util::*;
use macros::*;

fn main() {
  // Runtime spawns an i/o thread + others + manages task dispatch for us
  let args: Vec<String> = std::env::args().collect();
  if args.len() > 1 {
    client::event_client(&args);
  }
  else {
    let rt = tokio::runtime::Builder::new_multi_thread()
      .enable_all()
      .worker_threads(2)
      .build()
      .expect("Could not build tokio runtime!");

    rt.block_on(eventmgr());
  }
}

// Each task spawned in eventmgr() has two interval ticks - the regular one, and a low-power one
// which ticks less often. During make_cpu_governor_decisions(), if we go to powersave we avoid having eventmgr itself
// consume significant CPU by ticking less often.
static IN_POWERSAVE_MODE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

static LID_IS_CLOSED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

// We record CPU runtime data & print every 60s or so
lazy_static::lazy_static! {
    static ref AGG: std::sync::Mutex<std::collections::HashMap<&'static str, std::time::Duration>> =
        std::sync::Mutex::new(std::collections::HashMap::new());
}

async fn eventmgr() {
  notify("Beginning eventmgr event loop...").await;

  let mut tasks = vec![
    PersistentAsyncTask::new("handle_exit_signals",              ||{ tokio::task::spawn(instrument_async("handle_exit_signals", handle_exit_signals()) ) }),
    PersistentAsyncTask::new("handle_sway_msgs",                 ||{ tokio::task::spawn(instrument_async("handle_sway_msgs", handle_sway_msgs()) ) }),
    PersistentAsyncTask::new("poll_device_audio_playback",       ||{ tokio::task::spawn(instrument_async("poll_device_audio_playback", poll_device_audio_playback()) ) }),
    PersistentAsyncTask::new("handle_socket_msgs",               ||{ tokio::task::spawn(instrument_async("handle_socket_msgs", handle_socket_msgs()) ) }),
    PersistentAsyncTask::new("poll_downloads",                   ||{ tokio::task::spawn(instrument_async("poll_downloads", poll_downloads()) ) }),
    PersistentAsyncTask::new("poll_wallpaper_rotation",          ||{ tokio::task::spawn(instrument_async("poll_wallpaper_rotation", poll_wallpaper_rotation()) ) }),
    // PersistentAsyncTask::new("poll_check_glucose",               ||{ tokio::task::spawn(instrument_async("poll_check_glucose", poll_check_glucose()) ) }),
    PersistentAsyncTask::new("mount_disks",                      ||{ tokio::task::spawn(instrument_async("mount_disks", mount_disks()) ) }),
    PersistentAsyncTask::new("mount_net_shares",                 ||{ tokio::task::spawn(instrument_async("mount_net_shares", mount_net_shares()) ) }),
    PersistentAsyncTask::new("bump_cpu_for_performance_procs",   ||{ tokio::task::spawn(instrument_async("bump_cpu_for_performance_procs", bump_cpu_for_performance_procs()) ) }),
    PersistentAsyncTask::new("partial_resume_paused_procs",      ||{ tokio::task::spawn(instrument_async("partial_resume_paused_procs", partial_resume_paused_procs()) ) }),
    PersistentAsyncTask::new("mount_swap_files",                 ||{ tokio::task::spawn(instrument_async("mount_swap_files", mount_swap_files()) ) }),
    PersistentAsyncTask::new("turn_off_misc_lights",             ||{ tokio::task::spawn(instrument_async("turn_off_misc_lights", turn_off_misc_lights()) ) }),
    PersistentAsyncTask::new("update_dns_records",               ||{ tokio::task::spawn(instrument_async("update_dns_records", update_dns_records()) ) }),
    PersistentAsyncTask::new("run_disregarded_pvms",             ||{ tokio::task::spawn(instrument_async("run_disregarded_pvms", run_disregarded_pvms()) ) }),
    PersistentAsyncTask::new("run_package_downloader",           ||{ tokio::task::spawn(instrument_async("run_package_downloader", run_package_downloader()) ) }),
    PersistentAsyncTask::new("log_runtime_stats",                ||{ tokio::task::spawn(instrument_async("log_runtime_stats", log_runtime_stats()) ) }),
    PersistentAsyncTask::new("handle_lid_states",                ||{ tokio::task::spawn(instrument_async("handle_lid_states", handle_lid_states()) ) }),
  ];

  // Initialize any early-memory stuff
  once_at_startup_blocking().await;

  // Spawn one non-repeating task
  tokio::task::spawn(once_at_startup_nonblocking());

  // We check for failed tasks and re-start them every 6 seconds
  let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(6));
  interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
  let mut powersave_interval = tokio::time::interval(tokio::time::Duration::from_secs(26));
  powersave_interval.set_missed_tick_behavior(MissedTickBehavior::Delay);

  loop {
    if IN_POWERSAVE_MODE.load(std::sync::atomic::Ordering::Relaxed) {
      powersave_interval.tick().await;
    }
    else {
      interval.tick().await;
    }

    for i in 0..tasks.len() {
      tasks[i].ensure_running().await;
    }

  }
}

static PAUSED_PROC_CPU_CORE: once_cell::sync::Lazy<std::sync::atomic::AtomicUsize> = once_cell::sync::Lazy::new(||
  std::sync::atomic::AtomicUsize::new(0)
);

async fn once_at_startup_blocking() {
  // Set some defaults
  // Discovered the P14 super-E cores via:
  //   cargo install raw-cpuid --features cli
  //   for i in $(seq 0 22) ; do echo $i ; taskset -c "$i" cpuid > "/tmp/cpu_$i.txt" ; done
  // and manually diffing eg /tmp/cpu_0.txt w/ /tmp/cpu_20.txt
  // Biggest indicator of the super-E core was a lack of L3 cache.
  // Regular E cores are 12-19 inclusive and these do have L3 cache.
  if num_cpus::get() > 20 {
    PAUSED_PROC_CPU_CORE.store(20, std::sync::atomic::Ordering::SeqCst);
  }

  // TODO possible hardware query!


}

async fn once_at_startup_nonblocking() {
  // Set Wifi in high-power mode w/
  // sudo iw reg set BO ; sudo iwconfig wlan0 txpower 30
  dump_error!(
    tokio::process::Command::new("sudo")
      .args(&["-n", "iw", "reg", "set", "BO" ])
      .status()
      .await
  );
  let wlan_names = &["wlan0"];
  for wlan_name in wlan_names {
    dump_error!(
      tokio::process::Command::new("sudo")
        .args(&["-n", "iwconfig", wlan_name, "txpower", "29" ])
        .status()
        .await
    );
  }
}


const NOTIFICATION_TIMEOUT_MS: u32 = 1800;

async fn notify(msg: &str) {
  println!("{}", msg);
  dump_error!(
    notify_rust::Notification::new()
      //.summary("EventMgr")
      .body(msg)
      //.icon("firefox")
      .timeout(notify_rust::Timeout::Milliseconds(NOTIFICATION_TIMEOUT_MS)) //milliseconds
      .show()
  );
}

fn notify_sync(msg: &str) {
  println!("{}", msg);
  dump_error!(
    notify_rust::Notification::new()
      //.summary("EventMgr")
      .body(msg)
      //.icon("firefox")
      .timeout(notify_rust::Timeout::Milliseconds(NOTIFICATION_TIMEOUT_MS)) //milliseconds
      .show()
  );
}


async fn notify_icon(icon: &str, msg: &str) {
  println!("{}", msg);
  dump_error!(
    notify_rust::Notification::new()
      //.summary("EventMgr")
      .body(msg)
      .icon(icon)
      .timeout(notify_rust::Timeout::Milliseconds(NOTIFICATION_TIMEOUT_MS)) //milliseconds
      .show()
  );
}

fn notify_icon_sync(icon: &str, msg: &str) {
  println!("{}", msg);
  dump_error!(
    notify_rust::Notification::new()
      //.summary("EventMgr")
      .body(msg)
      .icon(icon)
      .timeout(notify_rust::Timeout::Milliseconds(NOTIFICATION_TIMEOUT_MS)) //milliseconds
      .show()
  );
}


async fn notify_icon_only(icon: &str) {
  dump_error!(
    tokio::process::Command::new("dunstify")
      .args(&["--appname=icononly", "--timeout=850", format!("--icon={}", icon).as_str(), "icon only" ])
      .status()
      .await
  );
}

fn notify_icon_only_sync(icon: &str) {
  dump_error!(
    std::process::Command::new("dunstify")
      .args(&["--appname=icononly", "--timeout=850", format!("--icon={}", icon).as_str(), "icon only" ])
      .status()
  );
}

async fn clear_notifications() {
  dump_error!(
    tokio::process::Command::new("dunstctl")
      .args(&["close-all"])
      .status()
      .await
  );
}

fn clear_notifications_sync() {
  dump_error!(
      std::process::Command::new("dunstctl")
        .args(&["close-all"])
        .status()
    );
}

async fn handle_exit_signals() {
  let mut int_stream = dump_error_and_ret!(
    tokio::signal::unix::signal(
      tokio::signal::unix::SignalKind::interrupt()
    )
  );
  let mut term_stream = dump_error_and_ret!(
    tokio::signal::unix::signal(
      tokio::signal::unix::SignalKind::terminate()
    )
  );
  loop {
    let want_shutdown: bool;
    tokio::select!{
      _sig_int = int_stream.recv() => { want_shutdown = true; }
      _sig_term = term_stream.recv() => { want_shutdown = true; }
    };
    if want_shutdown {
      println!("Got SIG{{TERM/INT}}, shutting down!");
      unpause_all_paused_pids().await;
      unmount_all_disks().await;
      // Allow spawned futures to complete...
      tokio::time::sleep( tokio::time::Duration::from_millis(500) ).await;
      println!("Goodbye!");
      std::process::exit(0);
    }
  }
}


async fn handle_sway_msgs() {
  // If we do not have I3SOCK or SWAYSOCK defined in env,
  // We must look them up manually.
  // See implementation from https://github.com/JayceFayne/swayipc-rs/blob/master/async/src/socket.rs
  if ! (std::env::var("I3SOCK").is_ok() || std::env::var("SWAYSOCK").is_ok()) {
    // Go fish
    println!("Do not have I3SOCK or SWAYSOCK defined, searching for a socket under /run/user or /tmp...");

    let vars_dirs_and_search_fragments = &[
      ("SWAYSOCK", "/run/user/1000", "sway-ipc"),
      ("I3SOCK",   "/tmp",           "ipc-socket"),
    ];

    let mut found_one = false;
    for (env_var_to_set, directory, search_fragment) in vars_dirs_and_search_fragments {
      if found_one {
        break;
      }
      let mut entries = async_walkdir::WalkDir::new(directory);
      while let Some(entry_result) = entries.next().await {
        if let Ok(entry) = entry_result {
          let name_s = entry.file_name().to_string_lossy().to_lowercase();
          if name_s.contains(search_fragment) {
            println!("Found {:?}", entry.path() );
            std::env::set_var(env_var_to_set, entry.path().into_os_string() );
            found_one = true;
            break;
          }
        }
      }
    }
  }

  let subs = [
      swayipc_async::EventType::Workspace,
      swayipc_async::EventType::Window,
  ];
  let mut events = dump_error_and_ret!( dump_error_and_ret!(swayipc_async::Connection::new().await).subscribe(subs).await );
  while let Some(event) = events.next().await {
    if let Ok(event) = event {
      match event {
        swayipc_async::Event::Window(window_evt) => {
          if window_evt.change == swayipc_async::WindowChange::Focus {
            let window_evt_container = window_evt.container.clone();
            let name = window_evt.container.name.clone().unwrap_or("".to_string());
            tokio::task::spawn(async move {
              on_window_focus(&name, &window_evt_container).await;
            });
          }
          else if window_evt.change == swayipc_async::WindowChange::Close {
            // Some of our scripts store per-window meta-data under /tmp/.twin/<window-id> - if that file exists AND this window ID is closed,
            // delete that data.
            let window_id_path = std::path::PathBuf::from(format!("/tmp/.twin/{}", window_evt.container.id));
            if window_id_path.exists() {
              dump_error!( std::fs::remove_file(&window_id_path) );
            }
          }
        }
        swayipc_async::Event::Workspace(workspace_evt) => {
          if workspace_evt.change == swayipc_async::WorkspaceChange::Focus {
            if let Some(focused_ws) = workspace_evt.current {
              let name = focused_ws.name.clone().unwrap_or("".to_string());
              tokio::task::spawn(async move {
                on_workspace_focus(&name).await;
              });
            }
          }

        }
        unhandled_evt => {
          println!("Unhandled sway event = {:?}", unhandled_evt);
        }
      }
    }
  }
}

// async fn run_sway_cmd<T: AsRef<str>>(cmd: T) {
//   if let Ok(mut conn) = swayipc_async::Connection::new().await {
//     dump_error_and_ret!( conn.run_command(cmd).await );
//   }
// }

async fn set_sway_wallpaper<T: AsRef<str>>(wallpaper: T) {
  let wallpaper = wallpaper.as_ref();
  /*
  if let Ok(mut conn) = swayipc_async::Connection::new().await {
    for output in dump_error_and_ret!( conn.get_outputs().await ) {
      // ALL monitors get wallpaper!
      let wallpaper_cmd = format!("output {} bg {} fill", output.name, wallpaper);
      println!("Running wallpaper cmd: {}", wallpaper_cmd);
      dump_error_and_cont!( conn.run_command(wallpaper_cmd).await );
    }
  }*/

  // Ensure pulse can connect!
  if ! std::env::var("DBUS_SESSION_BUS_ADDRESS").is_ok() {
    std::env::set_var("DBUS_SESSION_BUS_ADDRESS", "unix:path=/run/user/1000/bus");
  }

  if ! std::env::var("XDG_RUNTIME_DIR").is_ok() { // Just guessing here
    std::env::set_var("XDG_RUNTIME_DIR", "/run/user/1000");
  }

  if ! std::env::var("WAYLAND_DISPLAY").is_ok() { // Just guessing here
    for i in 0..99 {
      let wayland_disp_name = format!("wayland-{}", i);
      let wayland_lock_file = format!("/run/user/1000/wayland-{}.lock", i);
      if std::path::Path::new(&wayland_lock_file).exists() {
        std::env::set_var("WAYLAND_DISPLAY", &wayland_disp_name);
        break;
      }
    }
  }


  let mut swww_args: Vec<String> = vec![];
  swww_args.push("img".into());
  swww_args.push(wallpaper.into());

  swww_args.push("--transition-step".into());
  swww_args.push("2".into());
  swww_args.push("--transition-fps".into());
  swww_args.push("18".into());

  /*let rand_num = fastrand::usize(0..100);
  if rand_num < 20 {
    swww_args.push("--transition-type".into());
    swww_args.push("center".into());
  }
  else if rand_num < 40 {

  }
  else if rand_num < 60 {

  }
  else if rand_num < 80 {

  }
  else {

  }*/
  swww_args.push("--transition-type".into());
  swww_args.push("random".into());

  swww_args.push("--transition-bezier".into());
  swww_args.push(".81,.12,.84,.14".into());

  dump_error_and_ret!(
    tokio::process::Command::new("swww")
      .args(&swww_args)
      .status()
      .await
  );

}


static UTC_S_LAST_SEEN_FS_TEAM_FORTRESS: once_cell::sync::Lazy<std::sync::atomic::AtomicUsize> = once_cell::sync::Lazy::new(||
  std::sync::atomic::AtomicUsize::new(0)
);

static LAST_FOCUSED_WINDOW_NAME: once_cell::sync::Lazy<tokio::sync::RwLock<String>> = once_cell::sync::Lazy::new(||
  tokio::sync::RwLock::new( "".to_owned() )
);

static HAVE_HIGH_PERF_WINDOW_VISIBLE: once_cell::sync::Lazy<std::sync::atomic::AtomicBool> = once_cell::sync::Lazy::new(||
  std::sync::atomic::AtomicBool::new(false)
);
static HAVE_LOW_PERF_WINDOW_VISIBLE: once_cell::sync::Lazy<std::sync::atomic::AtomicBool> = once_cell::sync::Lazy::new(||
  std::sync::atomic::AtomicBool::new(false)
);


async fn on_window_focus(window_name: &str, sway_node: &swayipc_async::Node) {
  //print_time!("on_window_focus"); // recorded 8 -> 14ms

  println!("Window focused = {:?}", window_name );

  {
    let mut last_focused_window_name = LAST_FOCUSED_WINDOW_NAME.write().await;
    *last_focused_window_name = window_name.to_owned();
  }

  darken_kbd_if_video_focused_and_audio_playing().await;

  let lower_window = window_name.to_lowercase();
  if is_tf2_window(&lower_window) {
    //make_cpu_governor_decisions(Some(CPU_GOV_PERFORMANCE), None).await;
    HAVE_HIGH_PERF_WINDOW_VISIBLE.store(true, std::sync::atomic::Ordering::SeqCst);
    HAVE_LOW_PERF_WINDOW_VISIBLE.store(false, std::sync::atomic::Ordering::SeqCst);
    make_cpu_governor_decisions(None, None).await;
    unpause_proc("tf_linux64").await;
    UTC_S_LAST_SEEN_FS_TEAM_FORTRESS.store(
      std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).expect("Time travel!").as_secs() as usize,
      std::sync::atomic::Ordering::Relaxed
    );
  }
  else {
    if lower_window.contains(" - mpv") {
      HAVE_HIGH_PERF_WINDOW_VISIBLE.store(false, std::sync::atomic::Ordering::SeqCst);
      HAVE_LOW_PERF_WINDOW_VISIBLE.store(true, std::sync::atomic::Ordering::SeqCst);
      //make_cpu_governor_decisions(Some(CPU_GOV_POWERSAVE), None).await;
      make_cpu_governor_decisions(None, None).await;
    }
    else if lower_window.contains("spice display") {
      //make_cpu_governor_decisions(Some(CPU_GOV_PERFORMANCE), None).await; // bump for VM
      HAVE_HIGH_PERF_WINDOW_VISIBLE.store(true, std::sync::atomic::Ordering::SeqCst);
      HAVE_LOW_PERF_WINDOW_VISIBLE.store(false, std::sync::atomic::Ordering::SeqCst);
      make_cpu_governor_decisions(None, None).await;
    }
    else {
      HAVE_HIGH_PERF_WINDOW_VISIBLE.store(false, std::sync::atomic::Ordering::SeqCst);
      HAVE_LOW_PERF_WINDOW_VISIBLE.store(false, std::sync::atomic::Ordering::SeqCst);
      make_cpu_governor_decisions(None, None).await;
    }

    // Only pause IF we've seen team fortress fullscreen in the last 15 minutes / 900s
    let seconds_since_saw_tf2_fs = (std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).expect("Time travel!").as_secs() as usize) - UTC_S_LAST_SEEN_FS_TEAM_FORTRESS.load(std::sync::atomic::Ordering::Relaxed);
    if seconds_since_saw_tf2_fs < 900 {
      // AND we're not playing audio
      /*
      let currently_playing_audio = CURRENTLY_PLAYING_AUDIO.load(std::sync::atomic::Ordering::SeqCst);
      if ! currently_playing_audio {
        pause_proc("tf_linux64").await;
      }
      */
      pause_proc("tf_linux64").await;
    }

  }


}

static CURRENT_KBD_LIGHT_VAL: once_cell::sync::Lazy<std::sync::atomic::AtomicU32> = once_cell::sync::Lazy::new(||
  std::sync::atomic::AtomicU32::new(5)
);

// Increments up to 2 on audio playing, decrements down to 0 on no audio. We turn
// keyboard on when no audio + CURRENT_KBD_AUDIO_SEMAPHOR == 0
static CURRENT_KBD_AUDIO_SEMAPHOR: once_cell::sync::Lazy<std::sync::atomic::AtomicU32> = once_cell::sync::Lazy::new(||
  std::sync::atomic::AtomicU32::new(0)
);

async fn darken_kbd_if_video_focused_and_audio_playing() {

  // Override all below logic w/ file
  if std::path::Path::new("/tmp/light-kbd").exists() {
    set_kbd_light(1).await;
    return;
  }
  if std::path::Path::new("/tmp/dark-kbd").exists() {
    set_kbd_light(0).await;
    return;
  }


  let currently_playing_audio = CURRENTLY_PLAYING_AUDIO.load(std::sync::atomic::Ordering::SeqCst);
  let audio_semaphor = CURRENT_KBD_AUDIO_SEMAPHOR.load(std::sync::atomic::Ordering::SeqCst);
  if ! currently_playing_audio {
    if audio_semaphor == 0 {
      set_kbd_light(1).await;
    }
    else {
      // Go down by 1
      CURRENT_KBD_AUDIO_SEMAPHOR.store(audio_semaphor.saturating_sub(1), std::sync::atomic::Ordering::SeqCst);
    }
    return;
  }
  // We are playing audio!
  let focused_win_name = LAST_FOCUSED_WINDOW_NAME.read().await;
  let focused_win_name = focused_win_name.to_lowercase();

  let is_video = focused_win_name.contains("youtube") ||
                 focused_win_name.contains("mpv") ||
                 false;//focused_win_name.contains("mozilla firefox");

  if is_video {
    set_kbd_light(0).await;
    // Increment by 1 until we hit 2
    if audio_semaphor < 2 {
      CURRENT_KBD_AUDIO_SEMAPHOR.store(audio_semaphor + 1, std::sync::atomic::Ordering::SeqCst);
    }
  }
  else {
    if audio_semaphor == 0 {
      set_kbd_light(1).await;
    }
    else {
      // Go down by 1
      CURRENT_KBD_AUDIO_SEMAPHOR.store(audio_semaphor.saturating_sub(1), std::sync::atomic::Ordering::SeqCst);
    }
  }

}

async fn set_kbd_light(level: u32) {
  // let current_level = CURRENT_KBD_LIGHT_VAL.load(std::sync::atomic::Ordering::Relaxed);
  // if current_level == level {
  //   return;
  // }

  if let Ok(led_devices) = bulbb::misc::LedDevice::get_all_led_devices() {
    for ld in led_devices {
      if let Some(bulbb::misc::LedFunction::KbdBacklight) = ld.info.function {
        dump_error_and_cont!( ld.set_brightness(level) );
      }
    }
  }

  CURRENT_KBD_LIGHT_VAL.store(level, std::sync::atomic::Ordering::Relaxed);

}

static CURRENTLY_PLAYING_AUDIO: once_cell::sync::Lazy<std::sync::atomic::AtomicBool> = once_cell::sync::Lazy::new(||
  std::sync::atomic::AtomicBool::new(false)
);

// From   pactl list | grep -i monitor
const PULSE_AUDIO_MONITOR_DEVICE_NAMES:  &'static [&'static str] = &[
  // "alsa_output.usb-Generic_USB_Audio_201701110001-00.analog-stereo.monitor", // desk headphones via cable
  // "alsa_output.pci-0000_00_1f.3.analog-stereo", // laptop speakers
  // "bluez_output.F4_9D_8A_D2_E3_01.1.monitor", // Bluetooth headphones
  "alsa_output.usb-Generic_USB_Audio_201701110001-00", // desk headphones via cable
  "alsa_output.pci-0000_00_1f.3", // laptop speakers
  "bluez_output.F4_9D_8A_D2_E3_01.1", // Bluetooth headphones
];

async fn poll_device_audio_playback() {
  // use alsa::pcm::*;
  // use alsa::{Direction, ValueOr, Error};
  use libpulse_simple_binding::Simple;
  use libpulse_binding::{stream,sample};

  // Ensure pulse can connect!
  if ! std::env::var("DBUS_SESSION_BUS_ADDRESS").is_ok() {
    std::env::set_var("DBUS_SESSION_BUS_ADDRESS", "unix:path=/run/user/1000/bus");
  }

  if ! std::env::var("XDG_RUNTIME_DIR").is_ok() { // Just guessing here
    std::env::set_var("XDG_RUNTIME_DIR", "/run/user/1000");
  }

  let mut interval = tokio::time::interval(tokio::time::Duration::from_millis(1800));
  interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
  let mut powersave_interval = tokio::time::interval(tokio::time::Duration::from_millis(12200));
  powersave_interval.set_missed_tick_behavior(MissedTickBehavior::Delay);

  interval.tick().await; // Wait 1 tick before recording

  // Also attempt to make a ton of files world-writeable
  dump_error_and_ret!(
    tokio::process::Command::new("sudo")
      .args(&["-n", "find", "/sys", "-iname", "brightness", "-exec", "chmod", "a+rw", "{}", ";" ])
      .status()
      .await
  );

  let audio_is_playing_flag_file = std::path::Path::new("/tmp/eventmgr-audio-is-playing");

  loop {
    if IN_POWERSAVE_MODE.load(std::sync::atomic::Ordering::Relaxed) {
      powersave_interval.tick().await;
    }
    else {
      interval.tick().await;
    }


    dump_error_and_ret!( tokio::task::spawn_blocking(move || {

        // Run pw-dump and capture its JSON output
        let output = std::process::Command::new("pw-dump")
            .output()
            .expect("Failed to run pw-dump");
        match serde_json::from_slice::<serde_json::Value>(&output.stdout) {
          Ok(data) => {

            let mut node_states = std::collections::HashMap::<String, String>::new();
            if let Some(array) = data.as_array() {
              for obj in array {
                  // Only consider Nodes
                  if obj.get("type").and_then(serde_json::Value::as_str) == Some("PipeWire:Interface:Node") {
                      let info = obj.get("info").and_then(serde_json::Value::as_object);
                      if let Some(info) = info {
                          let name = info
                              .get("props")
                              .and_then(|p| p.get("node.name"))
                              .and_then(serde_json::Value::as_str)
                              .unwrap_or("<unknown>");
                          let state = info
                              .get("state")
                              .and_then(serde_json::Value::as_str)
                              .unwrap_or("<unknown>");
                          //println!("{:<50}  state: {}", name, state);
                          node_states.insert(name.to_string(), state.to_string());
                      }
                  }
              }
          }

          let mut active_devices: [bool; 8] = [false; 8];

          for (i, monitor_dev_name) in PULSE_AUDIO_MONITOR_DEVICE_NAMES.iter().enumerate() {
            if let Some(dev_state) = node_states.get(*monitor_dev_name) {
              match dev_state.as_str() {
                "suspended" | "idle" => {
                  if i < active_devices.len() {
                    active_devices[i] = false;
                  }
                }
                other => {
                  if i < active_devices.len() {
                    active_devices[i] = true;
                  }
                }
              }
            }
          }

          let mut at_least_one_device_active = false;
          for ad in active_devices.iter() {
            if *ad {
              at_least_one_device_active = true;
            }
          }
          CURRENTLY_PLAYING_AUDIO.store(at_least_one_device_active, std::sync::atomic::Ordering::SeqCst);

          // Also create a file at /tmp/eventmgr-audio-is-playing for other utilities to use
          if at_least_one_device_active && !audio_is_playing_flag_file.exists() {
            dump_error!(std::fs::write(audio_is_playing_flag_file, b"y"));
          }
          else if !at_least_one_device_active && audio_is_playing_flag_file.exists() {
            dump_error!(std::fs::remove_file(audio_is_playing_flag_file));
          }

        }
        Err(e) => {
          eprintln!("pw-dump JSON parse {:?}", e);
        }
      }

    }).await );


    darken_kbd_if_video_focused_and_audio_playing().await;

  }

}


async fn on_workspace_focus(workspace_name: &str) {
  println!("Workspace focused = {:?}", workspace_name );

  // if workspace_name.contains("7") ||workspace_name.contains("8") || workspace_name.contains("9") {
  //   // Likely second monitor, set black BG
  //   dump_error!(
  //     tokio::process::Command::new("swaymsg")
  //       .args(&["output DP-1 bg #000000 solid_color"])
  //       .status()
  //       .await
  //   );
  //   dump_error!(
  //     tokio::process::Command::new("swaymsg")
  //       .args(&["output DP-2 bg #000000 solid_color"])
  //       .status()
  //       .await
  //   );
  // }
}

// static LAST_CPU_LEVEL: once_cell::sync::Lazy<(&str, usize)> = once_cell::sync::Lazy::new(|| (CPU_GOV_ONDEMAND, 0) );


// TODO handle lowering CPU level over time / desktop activity


async fn handle_socket_msgs() {
  if std::path::Path::new(SERVER_SOCKET).exists() {
    dump_error_and_ret!(
      std::fs::remove_file(SERVER_SOCKET)
    );
  }

  let server = dump_error_and_ret!( tokio::net::UnixDatagram::bind(SERVER_SOCKET) );
  let mut msg_buf = vec![0u8; 4096];
  let mut msg_size: usize = 0;

  loop {
    if let Ok((size, _addr)) = server.recv_from(&mut msg_buf).await {
      msg_size = size;
    }
    if msg_size > 0 {
      // Handle message!
      let msg_bytes = &msg_buf[0..msg_size];
      match ciborium::de::from_reader::<ciborium::Value, &[u8]>(msg_bytes) {
        Ok(msg_val) => {
          println!("[ handle_socket_msgs ] Got message: {:?}", &msg_val );
          if let ciborium::value::Value::Array(vec_of_vals) = msg_val {
            if vec_of_vals.len() == 2 {
              if let ciborium::value::Value::Text(arg1_string) = &vec_of_vals[1] {
                do_simple_client_arg1(&arg1_string).await;
              }
            }
          }
        }
        Err(e) => {
          eprintln!("CBOR decoding error: {:?}", e);
        }
      }
      // clear message buffer
      msg_size = 0;
    }
  }

}

static KEYBOARD_IS_6S_INACTIVE: once_cell::sync::Lazy<std::sync::atomic::AtomicBool> = once_cell::sync::Lazy::new(||
  std::sync::atomic::AtomicBool::new(false)
);
static KEYBOARD_IS_30S_INACTIVE: once_cell::sync::Lazy<std::sync::atomic::AtomicBool> = once_cell::sync::Lazy::new(||
  std::sync::atomic::AtomicBool::new(false)
);

async fn do_simple_client_arg1(arg: &str) {

  println!("do_simple_client_arg1({:?})", arg);

  if arg == "keyboard-is-inactive-6s" {
    KEYBOARD_IS_6S_INACTIVE.store(true, std::sync::atomic::Ordering::SeqCst);
    make_cpu_governor_decisions(None, None).await;
  }
  else if arg == "keyboard-is-inactive-30s" {
    KEYBOARD_IS_30S_INACTIVE.store(true, std::sync::atomic::Ordering::SeqCst);
    make_cpu_governor_decisions(None, None).await;
  }
  else if arg == "keyboard-is-active" {
    KEYBOARD_IS_6S_INACTIVE.store(false, std::sync::atomic::Ordering::SeqCst);
    KEYBOARD_IS_30S_INACTIVE.store(false, std::sync::atomic::Ordering::SeqCst);
    make_cpu_governor_decisions(None, None).await;
  }
}


pub const DOWNLOAD_QUARANTINE_SECS: u64 = (24 + 12) * (60 * 60);
pub const QUARANTINE_DELETE_SECS: u64 = (3 * 24) * (60 * 60);

async fn poll_downloads() {
  let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(45));
  interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
  let mut powersave_interval = tokio::time::interval(tokio::time::Duration::from_secs(75));
  powersave_interval.set_missed_tick_behavior(MissedTickBehavior::Delay);

  let mut unzipped_files = std::collections::HashSet::<String>::new();

  loop {
    if IN_POWERSAVE_MODE.load(std::sync::atomic::Ordering::Relaxed) {
      powersave_interval.tick().await;
    }
    else {
      interval.tick().await;
    }

    if !std::path::Path::new("/j/downloads/q").exists() {
      dump_error_and_ret!( tokio::fs::create_dir_all("/j/downloads/q").await );
    }

    // First quarantine downloads
    let mut entries = dump_error_and_ret!( tokio::fs::read_dir("/j/downloads").await );
    while let Some(entry) = dump_error_and_ret!( entries.next_entry().await ) {
      if entry.file_name() == "q" || entry.file_name() == "s" {
        continue;
      }
      if let Ok(metadata) = entry.metadata().await {
        if let Ok(time) = metadata.modified() {
          if let Ok(duration) = time.elapsed() {
            let age_s = duration.as_secs();
            if age_s > DOWNLOAD_QUARANTINE_SECS {
              println!("Quarantining {:.2}-hour-old file {}", (age_s as f64 / (60.0 * 60.0) ), entry.file_name().to_string_lossy());
              let dst_file = std::path::Path::new("/j/downloads/q").join( entry.file_name() );
              dump_error_and_cont!( tokio::fs::rename(entry.path(), dst_file).await );
            }
          }
        }
      }

    }

    // Now scan quarantined files and delete old ones
    let mut entries = dump_error_and_ret!( tokio::fs::read_dir("/j/downloads/q").await );
    while let Some(entry) = dump_error_and_ret!( entries.next_entry().await ) {
      if let Ok(metadata) = entry.metadata().await {
        if let Ok(time) = metadata.modified() {
          if let Ok(duration) = time.elapsed() {
            let age_s = duration.as_secs();
            if age_s > QUARANTINE_DELETE_SECS {
              if metadata.is_dir() {
                println!("Deleting {:.2}-hour-old directory {}", (age_s as f64 / (60.0 * 60.0) ), entry.file_name().to_string_lossy());
                dump_error_and_ret!( tokio::fs::remove_dir_all( entry.path() ).await );
              }
              else {
                println!("Deleting {:.2}-hour-old file {}", (age_s as f64 / (60.0 * 60.0) ), entry.file_name().to_string_lossy());
                dump_error_and_ret!( tokio::fs::remove_file( entry.path() ).await );
              }
            }
          }
        }
      }
    }

    // Next Unzip .zip files
    let mut entries = dump_error_and_ret!( tokio::fs::read_dir("/j/downloads").await );
    while let Some(entry) = dump_error_and_ret!( entries.next_entry().await ) {
      let fname = entry.file_name();
      let fname = fname.to_string_lossy();
      let fname_string = fname.to_string();
      let mut extracted_something = false;

      if unzipped_files.contains(&fname_string) {
        continue;
      }

      if fname.to_lowercase().ends_with(".zip") && dump_error_and_ret!( entry.path().metadata() ).len() > 2 {
        // See if dir exists
        let unzip_dir = std::path::Path::new("/j/downloads").join( fname.replace(".zip", "") );
        if !unzip_dir.exists() {
          // Unzip it!

          notify(format!("Unzipping {:?} to {:?}", entry.path(), unzip_dir).as_str()).await;

          dump_error_and_ret!(
            tokio::process::Command::new("mkdir")
              .args(&["-p", unzip_dir.to_string_lossy().borrow() ])
              .status()
              .await
          );

          dump_error_and_ret!(
            tokio::process::Command::new("unzip")
              .args(&[entry.path().to_string_lossy().borrow(), "-d", unzip_dir.to_string_lossy().borrow() ])
              .status()
              .await
          );

          extracted_something = true;
          unzipped_files.insert(fname_string);
        }
      }
      else if fname.to_lowercase().ends_with(".7z") && dump_error_and_ret!( entry.path().metadata() ).len() > 2 {
        // See if dir exists
        let unzip_dir = std::path::Path::new("/j/downloads").join( fname.replace(".7z", "") );
        if !unzip_dir.exists() {
          // Unzip it!

          notify(format!("Unzipping {:?}", entry.path()).as_str()).await;

          /*dump_error_and_ret!(
            tokio::process::Command::new("mkdir")
              .args(&["-p", unzip_dir.to_string_lossy().borrow() ])
              .status()
              .await
          );*/

          /*dump_error_and_ret!(
            tokio::process::Command::new("7z")
              .args(&["e", &format!("{}", unzip_dir.to_string_lossy()), entry.path().to_string_lossy().borrow() ])
              .status()
              .await
          );*/

          dump_error_and_ret!(
            tokio::process::Command::new("7z")
              .current_dir("/j/downloads")
              .args(&["e", entry.path().to_string_lossy().borrow() ])
              .status()
              .await
          );

          extracted_something = true;
          unzipped_files.insert(fname_string);

        }
      }

      if extracted_something {
        /* todo re-design w/o touching _all_ files, just those that share a zip prefix name or something
        // Touch _all_ files with mtimes older than the beginning of this loop; this protects against
        let mut entries = dump_error_and_ret!( tokio::fs::read_dir("/j/downloads").await );
        while let Some(entry) = dump_error_and_ret!( entries.next_entry().await ) {
          let p = entry.path();
          let s = p.to_string_lossy();
          let s: &str = s.borrow();
          dump_error_and_ret!(
            tokio::process::Command::new("touch")
              .arg(s)
              .status()
              .await
          );
        }
        */
      }
    }



  }
}




// Higher numbers selected more often
static WALLPAPER_DIR_WEIGHTS: phf::Map<&'static str, usize> = phf::phf_map! {
  "/j/photos/wallpaper/ocean" => 200,
  "/j/photos/wallpaper/earth" => 100,
  "/j/photos/wallpaper/sky"   => 20,
  "/j/photos/wallpaper/space" => 20,
};

async fn poll_wallpaper_rotation() {
  //let mut interval = tokio::time::interval(tokio::time::Duration::from_secs( 180 ));
  let mut small_interval = tokio::time::interval(tokio::time::Duration::from_millis( 1200 ));
  small_interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
  let mut small_powersave_interval = tokio::time::interval(tokio::time::Duration::from_millis( 8200 ));
  small_powersave_interval.set_missed_tick_behavior(MissedTickBehavior::Delay);

  let mut weights_total: usize = 0;
  for (_, weight) in WALLPAPER_DIR_WEIGHTS.entries() {
    weights_total += weight;
  }

  // We create /tmp/do-wallpaper to force a wallpaper rotation as soon as we login/startup,
  // as opposed to waiting 90 seconds for one.
  dump_error!( tokio::fs::write("/tmp/do-wallpaper", "-".as_bytes()).await );

  loop {

    while LID_IS_CLOSED.load(std::sync::atomic::Ordering::Relaxed) {
      small_powersave_interval.tick().await; // Do nothing if LID_IS_CLOSED
    }

    if IN_POWERSAVE_MODE.load(std::sync::atomic::Ordering::Relaxed) {
      for _ in 0..18 {
        small_powersave_interval.tick().await;
        if tokio::fs::try_exists("/tmp/do-wallpaper").await.unwrap_or(false) {
          dump_error!( tokio::fs::remove_file("/tmp/do-wallpaper").await );
          break;
        }
      }
    }
    else {
      for _ in 0..70 {
        small_interval.tick().await;
        if tokio::fs::try_exists("/tmp/do-wallpaper").await.unwrap_or(false) {
          dump_error!( tokio::fs::remove_file("/tmp/do-wallpaper").await );
          break;
        }
      }
    }


    let mut picked_wp_dir = "".to_string();

    // First check if we have an override in /tmp/wallpaper-folder
    if let Ok(override_wp_dir) = tokio::fs::read_to_string("/tmp/wallpaper-folder").await {
      picked_wp_dir = override_wp_dir.trim().to_string();
      // if picked_wp_dir does not exist, append to /j/photos/wallpaper/ until it does?
      if ! std::path::Path::new(&picked_wp_dir).exists() {
        picked_wp_dir = format!("/j/photos/wallpaper/{}", picked_wp_dir);
      }
    }
    else {
      for _ in 0..500 { // "kinda" an infinite loop, exiting when a dir has been picked
        for (wp_dir, dir_weight) in WALLPAPER_DIR_WEIGHTS.entries() {
          let rand_num = fastrand::usize(0..weights_total);
          if rand_num <= *dir_weight {
            picked_wp_dir = wp_dir.to_string();
            break;
          }
        }
        if picked_wp_dir.len() > 0 {
          break;
        }
      }
    }

    if picked_wp_dir.len() > 0 {
      // search for a file at random
      let mut entries = dump_error_and_ret!( tokio::fs::read_dir(picked_wp_dir).await );
      let mut entry_paths = vec![];
      while let Some(entry) = dump_error_and_ret!( entries.next_entry().await ) {
        entry_paths.push( entry.path() );
      }
      if entry_paths.len() > 0 {
        let i = fastrand::usize(0..entry_paths.len());
        set_sway_wallpaper(
          entry_paths[i].to_string_lossy()
        ).await;
      }
    }


  }
}

async fn poll_check_glucose() {
  let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(120));
  interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
  loop {
    interval.tick().await;

    // glucose /tmp/glucose.txt
    dump_error_and_ret!(
      tokio::process::Command::new("/j/bin/glucose")
        .args(&["/tmp/glucose.txt"])
        .status()
        .await
    );

  }
}


static MOUNT_DISKS: phf::Map<&'static str, &[(&'static str, &'static str)] > = phf::phf_map! {
  /*"/dev/disk/by-partuuid/8f3ca68c-d031-2d41-849c-be5d9602e920" =>
    &[
      ("/mnt/azure-data", "defaults,rw,autodefrag,compress=zstd:11,commit=300,nodatasum"),
      // sudo rmdir /mnt/azure-data/swap-files ; sudo btrfs subvolume create '/mnt/azure-data/@swap-files' ; sudo btrfs property set /mnt/azure-data/swap-files compression none
      ("/mnt/azure-data/swap-files", "defaults,rw,noatime,nodatacow,subvol=@swap-files,nodatasum")
    ],
    // ^^ compress=lzo is fast w/o huge compression ratios, zstd:9 is slower with better ratio. high commit seconds means disk writes are less common.
  */

  //"/dev/disk/by-partuuid/53da446a-2409-ca42-8337-12389dc70563" =>  // Retired for now
  //  &[("/mnt/scratch", "auto,rw,noatime,data=writeback,barrier=0,nobh,errors=remount-ro")],

  "/dev/disk/by-partuuid/e08214f5-cfc5-4252-afee-505dfcd23808" =>
    &[
      ("/mnt/scratch", "defaults,rw,autodefrag,compress=zstd:11,commit=300,nodatasum"),
      // sudo rmdir /mnt/scratch/swap-files ; sudo btrfs subvolume create '/mnt/scratch/@swap-files' ; sudo btrfs property set /mnt/scratch/swap-files compression none
      ("/mnt/scratch/swap-files", "defaults,rw,noatime,nodatacow,subvol=@swap-files,nodatasum")
    ],

  "/dev/disk/by-partuuid/435cfadf-6a6e-4acf-a784-ab3f792ee8c6" =>
    &[("/mnt/wda", "auto,rw")],

  "/dev/disk/by-partuuid/ee209a96-9170-534a-9ba2-ea0a34ac156e" =>
    &[("/mnt/wdb", "auto,rw")],

  // Not a block device, but we special-case any "options string with space chars"
  // and run the value in a root /bin/sh -c shell to allow 3rd-party utilities like ifuse to handle mounting.
  /*"/sys/class/power_supply/apple_mfi_fastcharge" =>
    &[
      ("/mnt/iphone-root", "ifuse -o allow_other,rw /mnt/iphone-root"),
      ("/mnt/iphone-vox",  "ifuse --documents com.coppertino.VoxMobile -o allow_other,rw /mnt/iphone-vox"),
      // ("/mnt/iphone-vox.has-been-synced",  "sleep 0.5 ; mount | grep -q iphone-vox && sudo -u jeffrey rsync -avxHAX --no-owner --no-group --no-perms --size-only --progress /j/music/ /mnt/iphone-vox/ ")
    ],*/

  "/dev/disk/by-label/ai-models" =>
    &[
      ("/mnt/ai-models.has-been-ntfsfixed", "sleep 0.1 ; sudo ntfsfix /dev/disk/by-label/ai-models || true"),
      ("/mnt/ai-models", "defaults,rw,uid=1000,gid=1000"),
      // sudo ntfsfix /dev/disk/by-label/ai-models
    ]

};

async fn mount_disks() {
  let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(4));
  interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
  let mut powersave_interval = tokio::time::interval(tokio::time::Duration::from_secs(9));
  powersave_interval.set_missed_tick_behavior(MissedTickBehavior::Delay);

  // First, we use rmdir to remove all empty directories that exist under /mnt/
  let mut rmdir_cmd = vec!["-n", "rmdir"];
  if let Ok(info) = mountinfo::MountInfo::new() {
    for (disk_block_device, disk_mount_items) in MOUNT_DISKS.entries() {
      for (disk_mount_path, disk_mount_opts) in disk_mount_items.iter() {
        if ! is_mounted(&info, disk_mount_path).await {
          rmdir_cmd.push(disk_mount_path);
        }
      }
    }
  }

  if rmdir_cmd.len() > 2 {
    dump_error!(
      tokio::process::Command::new("sudo")
        .args(&rmdir_cmd)
        .status()
        .await
    );
  }

  loop {
    if IN_POWERSAVE_MODE.load(std::sync::atomic::Ordering::Relaxed) {
      powersave_interval.tick().await;
    }
    else {
      interval.tick().await;
    }

    if let Ok(info) = mountinfo::MountInfo::new() {
      for (disk_block_device, disk_mount_items) in MOUNT_DISKS.entries() {
        for (disk_mount_path, disk_mount_opts) in disk_mount_items.iter() {
          if std::path::Path::new(disk_block_device).exists() {
            if ! is_mounted(&info, disk_mount_path).await {
              let disk_mount_path_existed: bool;
              if ! std::path::Path::new(disk_mount_path).exists() {
                disk_mount_path_existed = false;
                println!("Because {:?} exists we are mounting {:?} with options {:?}", disk_block_device, disk_mount_path, disk_mount_opts);
                // Sudo create it
                dump_error!(
                  tokio::process::Command::new("sudo")
                    .args(&["-n", "mkdir", "-p", disk_mount_path])
                    .status()
                    .await
                );

                // Even if path previously exists, ensure jeffrey owns it
                dump_error!(
                  tokio::process::Command::new("sudo")
                    .args(&["-n", "chown", "jeffrey", disk_mount_path])
                    .status()
                    .await
                );
              }
              else {
                disk_mount_path_existed = true;
              }

              // We're still not mounted, do the mount!
              // If there are space chars in disk_mount_opts run as a command, else pass to "mount"
              if disk_mount_opts.contains(" ") && !disk_mount_path_existed {
                dump_error!(
                  tokio::process::Command::new("sudo")
                    .args(&["-n", "sh", "-c", disk_mount_opts])
                    .status()
                    .await
                );
              }
              else {
                dump_error!(
                  tokio::process::Command::new("sudo")
                    .args(&["-n", "mount", "-o", disk_mount_opts, disk_block_device, disk_mount_path])
                    .status()
                    .await
                );
              }

              // Sleep for 1 sec to allow mount, & continue if we aren't mounted to avoid secondary mount failures.
              tokio::time::sleep(tokio::time::Duration::from_millis(900)).await;
              if ! is_mounted(&info, disk_mount_path).await {
                continue;
              }

              // If it is mounted, clean Windorks nonsense by rm/rf-ing some files
              dump_error!(
                tokio::process::Command::new("sudo")
                  .args(&["-n", "rm", "-rf", format!("{}/$RECYCLE.BIN", disk_mount_path).as_str(), format!("{}/System Volume Information", disk_mount_path).as_str(), ])
                  .status()
                  .await
              );

              if std::path::Path::new("/System Volume Information").exists() { // do same for root drive
                dump_error!(
                  tokio::process::Command::new("sudo")
                    .args(&["-n", "rm", "-rf", "/$RECYCLE.BIN", "/System Volume Information", ])
                    .status()
                    .await
                );
              }

              // sudo rm -rf /\$RECYCLE.BIN /System\ Volume\ Information

            }
          }
          else {
            // Block device does NOT exist, remove mountpoint if it exists!
            // NOTE: if there is an i/o error (like with ifuse stuff) Path.exists() returns false!
            if std::path::Path::new(disk_mount_path).exists() || is_mounted(&info, disk_mount_path).await {
              println!("Because {:?} does not exist we are un-mounting {:?} (options {:?} unused)", disk_block_device, disk_mount_path, disk_mount_opts);
              dump_error!(
                tokio::process::Command::new("sudo")
                  .args(&["-n", "umount", disk_mount_path])
                  .status()
                  .await
              );
              dump_error!(
                tokio::process::Command::new("sudo")
                  .args(&["-n", "rmdir", "--parents", disk_mount_path])
                  .status()
                  .await
              );
            }
          }
        }
      }
    }
  }
}


static MOUNT_NET_SHARES: phf::Map<&'static str, &[(&'static str, &'static str)] > = phf::phf_map! {
  "machome.local" =>
    &[
      ("/mnt/machome/video",         "mount -t cifs -o username=jeffrey,password=$MACHOME_JEFF_PW,uid=1000,gid=1000 //machome.local/video /mnt/machome/video"),
      ("/mnt/machome/photo",         "mount -t cifs -o username=jeffrey,password=$MACHOME_JEFF_PW,uid=1000,gid=1000 //machome.local/photo /mnt/machome/photo"),
      ("/mnt/machome/miscellaneous", "mount -t cifs -o username=jeffrey,password=$MACHOME_JEFF_PW,uid=1000,gid=1000 //machome.local/miscellaneous /mnt/machome/miscellaneous"),
      ("/mnt/machome/family-photos", "mount -t cifs -o username=jeffrey,password=$MACHOME_JEFF_PW,uid=1000,gid=1000 //machome.local/family-photos /mnt/machome/family-photos"),
      ("/mnt/machome/web",           "mount -t cifs -o username=jeffrey,password=$MACHOME_JEFF_PW,uid=1000,gid=1000 //machome.local/web /mnt/machome/web"),
      ("/mnt/machome/homes_jeffrey", "mount -t cifs -o username=jeffrey,password=$MACHOME_JEFF_PW,uid=1000,gid=1000 //machome.local/homes/jeffrey /mnt/machome/homes_jeffrey"),
    ],
};

async fn mount_net_shares() {
  let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(32));
  interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
  let mut powersave_interval = tokio::time::interval(tokio::time::Duration::from_secs(98));
  powersave_interval.set_missed_tick_behavior(MissedTickBehavior::Delay);

  // First, we use rmdir to remove all empty directories that exist under /mnt/
  let mut rmdir_cmd = vec!["-n", "rmdir"];
  if let Ok(info) = mountinfo::MountInfo::new() {
    for (share_host, disk_mount_items) in MOUNT_NET_SHARES.entries() {
      for (disk_mount_path, disk_mount_opts) in disk_mount_items.iter() {
        if ! is_mounted(&info, disk_mount_path).await {
          if std::path::Path::new(disk_mount_path).exists() {
            rmdir_cmd.push(disk_mount_path);
          }
        }
      }
    }
  }

  if rmdir_cmd.len() > 2 {
    dump_error!(
      tokio::process::Command::new("sudo")
        .args(&rmdir_cmd)
        .status()
        .await
    );
  }

  // All secrets used by mount commands should read them here
  let mut network_subproc_env: std::collections::HashMap::<String, String> = std::collections::HashMap::<String, String>::new();

  match tokio::fs::read_to_string("/j/.cache/machome_jeff_pw").await {
    Ok(machome_jeff_pw) => {
      let machome_jeff_pw = machome_jeff_pw.trim().to_string();
      network_subproc_env.insert("MACHOME_JEFF_PW".into(), machome_jeff_pw);
    }
    Err(e) => {
      println!("e reading /j/.cache/machome_jeff_pw : {:?}", e);
      return;
    }
  }

  let network_subproc_env = network_subproc_env;

  const HOST_MAX_MISSED_PINGS: usize = 4;
  let mut host_missed_pings: std::collections::HashMap::<&'static str, usize> = std::collections::HashMap::<&'static str, usize>::new();

  loop {

    while LID_IS_CLOSED.load(std::sync::atomic::Ordering::Relaxed) {
      powersave_interval.tick().await; // Do nothing if LID_IS_CLOSED
    }

    if IN_POWERSAVE_MODE.load(std::sync::atomic::Ordering::Relaxed) {
      powersave_interval.tick().await;
    }
    else {
      interval.tick().await;
    }

    if let Ok(info) = mountinfo::MountInfo::new() {
      for (share_host, disk_mount_items) in MOUNT_NET_SHARES.entries() {
        let mut can_ping_share_host: Option<bool> = None;
        for (disk_mount_path, disk_mount_cmd) in disk_mount_items.iter() {
          if ! is_mounted(&info, disk_mount_path).await {
            // Can we ping share_host?
            if can_ping_share_host.is_none() {
              let dns_results = tokio::time::timeout(
                std::time::Duration::from_millis(12500),
                tokio::net::lookup_host(share_host)
              ).await;
              if let Ok(dns_results) = dns_results {
                if let Ok(dns_results) = dns_results {
                  let mut num_ips = 0;
                  for dns_result in dns_results {
                    println!("dns_result = {:?}", dns_result.ip() );
                    num_ips += 1;
                  }
                  if num_ips > 0 {
                    can_ping_share_host = Some(true); // got results
                    host_missed_pings.insert(share_host, 0); // clear missed pings
                  }
                  else {
                    println!("Got no data (inner) from tokio::net::lookup_host({})", &share_host);
                    can_ping_share_host = Some(false); // no data!
                  }
                }
                else {
                  //println!("Got no data from tokio::net::lookup_host({})", &share_host);
                  can_ping_share_host = Some(false); // no data!

                  // We try something old-school and slow to try and fix the situation;
                  let systemd_resolved_endpt = std::net::SocketAddr::new(std::net::IpAddr::V4(std::net::Ipv4Addr::new(127, 0, 0, 53)), 53);

                  let config = rsdns::clients::ClientConfig::with_nameserver(systemd_resolved_endpt);

                  if let Ok(mut client) = rsdns::clients::std::Client::new(config) {
                    if let Ok(rrset) = client.query_rrset::<rsdns::records::data::A>(share_host, rsdns::records::Class::IN) {
                      for ip_res in rrset.rdata {
                        //println!("DNS A rrset.ip_res = {:?}", ip_res);
                        can_ping_share_host = Some(true); // Got at least one result!
                      }
                    }
                    if let Ok(rrset) = client.query_rrset::<rsdns::records::data::Aaaa>(share_host, rsdns::records::Class::IN) {
                      for ip_res in rrset.rdata {
                        //println!("DNS AAAA rrset.ip_res = {:?}", ip_res);
                        can_ping_share_host = Some(true); // Got at least one result!
                      }
                    }
                  }

                }
              }
              else {
                println!("Timed out while trying to get tokio::net::lookup_host({})", &share_host);
                can_ping_share_host = Some(false); // timeout!
              }
            }
            if let Some(can_ping_share_host) = can_ping_share_host {
              if can_ping_share_host {
                // Not mounted but can ping, mount!

                println!("We can ping {} but have not yet mounted {}, mounting...", share_host, disk_mount_path);

                dump_error!(
                  tokio::process::Command::new("sudo")
                    .args(&["-n", "mkdir", "-p", disk_mount_path])
                    .status()
                    .await
                );

                dump_error!(
                  tokio::process::Command::new("sudo")
                    .args(&["-n", "chown", "jeffrey:jeffrey", disk_mount_path])
                    .status()
                    .await
                );

                dump_error!(
                  tokio::process::Command::new("sudo")
                    .envs(&network_subproc_env)
                    .args(&["--preserve-env", "-n", "sh", "-c", disk_mount_cmd])
                    .status()
                    .await
                );

              }
            }
          }
          else {
            // We are mounted, can we ping? If not then un-mount
            if can_ping_share_host.is_none() {
              let dns_results = tokio::time::timeout(
                std::time::Duration::from_millis(4500),
                tokio::net::lookup_host(share_host)
              ).await;
              if let Ok(dns_results) = dns_results {
                host_missed_pings.insert(share_host, 0); // Host is alive!
              }
              else {
                // Increment number of missed pings
                let num_missed_pings = host_missed_pings.get(share_host).unwrap_or(&0);
                host_missed_pings.insert(share_host, num_missed_pings + 1 );
              }
            }
            let num_missed_pings = host_missed_pings.get(share_host).unwrap_or(&0);
            let num_missed_pings = *num_missed_pings;
            if num_missed_pings >= HOST_MAX_MISSED_PINGS {
              println!("{} is mounted but host {} has disappeared (num_missed_pings={}), un-mounting!", disk_mount_path, share_host, num_missed_pings);

              dump_error!(
                tokio::process::Command::new("sudo")
                  .args(&["-n", "umount", disk_mount_path])
                  .status()
                  .await
              );

              dump_error!(
                tokio::process::Command::new("sudo")
                  .args(&["-n", "rmdir", "--parents", disk_mount_path])
                  .status()
                  .await
              );

            }

          }
        }
      }
    }

  }
}

static WANT_HIGH_CPU_STRING: once_cell::sync::Lazy<std::ffi::OsString> = once_cell::sync::Lazy::new(||
  std::ffi::OsStr::new("WANT_HIGH_CPU").into()
);

// This allows me to launch any process like "WANT_HIGH_CPU=t python some_script.py" and
// so long as that copy of python is running, bump_cpu_for_performance_procs will bump the CPU.
fn process_has_high_cpu_environ_set(p: &procfs::process::Process) -> bool {
  if let Ok(p_env) = p.environ() {
    return p_env.contains_key(WANT_HIGH_CPU_STRING.as_os_str());
  }
  return false;
}

static HAVE_HIGH_PERF_BG_PROC: once_cell::sync::Lazy<std::sync::atomic::AtomicBool> = once_cell::sync::Lazy::new(||
  std::sync::atomic::AtomicBool::new(false)
);

async fn bump_cpu_for_performance_procs() {
  let mut interval = tokio::time::interval(tokio::time::Duration::from_millis(4400));
  interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
  let mut powersave_interval = tokio::time::interval(tokio::time::Duration::from_millis(9000));
  powersave_interval.set_missed_tick_behavior(MissedTickBehavior::Delay);

  let mut not_high_perf_pids = std::collections::HashSet::<i32>::with_capacity(4096);
  let mut have_high_cpu = false;
  let mut tick_count = 0;
  loop {
    if IN_POWERSAVE_MODE.load(std::sync::atomic::Ordering::Relaxed) {
      powersave_interval.tick().await;
    }
    else {
      interval.tick().await;
    }

    let lid_is_closed = LID_IS_CLOSED.load(std::sync::atomic::Ordering::Relaxed);
    if lid_is_closed {
      continue; // Do not change CPU if lid closed!
    }

    tick_count += 1;
    if tick_count > 10000000 {
      tick_count = 0;
    }

    let mut want_high_cpu = false;
    let mut num_procs = 0;
    for p in dump_error_and_ret!( procfs::process::all_processes() ) {
      if let Ok(p) = p {
        num_procs += 1;
        if not_high_perf_pids.contains(&p.pid) {
          continue; // we already know!
        }
        if let Ok(p_exe) = p.exe() {
          if let Some(p_file_name) = p_exe.file_name() {
            let p_file_name = p_file_name.to_string_lossy();
            let heavy_p_running =
              p_file_name == "gcc" || p_file_name == "clang" ||
              p_file_name == "g++" || p_file_name == "clang++" ||
              p_file_name == "rustc" || p_file_name == "cargo" ||
              p_file_name == "make" || p_file_name == "pacman" ||
              p_file_name == "x86_64-w64-mingw32-gcc" ||
              p_file_name == "cc" || p_file_name == "cc1" ||
              process_has_high_cpu_environ_set(&p)
            ;
            if heavy_p_running {
              want_high_cpu = true;
              break;
            }
            else {
              not_high_perf_pids.insert(p.pid); // this PID will remain not-high-performance forever, so don't bother re-checking!
            }
          }
        }
      }
    }

    if tick_count % 5 == 0 {
      // every 5 seconds or so double-check if we have high CPU rather than trusting ourselves
      have_high_cpu = get_cpu().await == CPU_GOV_PERFORMANCE;
    }

    // Clear cache when we have >8x space known to be wasted!
    if not_high_perf_pids.len() > 8 * num_procs {
      not_high_perf_pids.clear();
    }

    HAVE_HIGH_PERF_BG_PROC.store(want_high_cpu, std::sync::atomic::Ordering::SeqCst);

    if want_high_cpu && !have_high_cpu {
      make_cpu_governor_decisions(Some(CPU_GOV_PERFORMANCE), None).await;
      //make_cpu_governor_decisions(None, None).await;
      have_high_cpu = true;
    }
    else if !want_high_cpu && have_high_cpu {
      make_cpu_governor_decisions(None, None).await;
      have_high_cpu = false;
    }

  }
}

fn is_tf2_window(lower_name: &str) -> bool {
  lower_name.contains("team") && lower_name.contains("fortress") && lower_name.contains("opengl")
}

fn is_high_perf_window(name: &str) -> bool {
  is_tf2_window(name)
}


static UTC_S_LAST_SET_PERFORMANCE_CPU: once_cell::sync::Lazy<std::sync::atomic::AtomicUsize> = once_cell::sync::Lazy::new(||
  std::sync::atomic::AtomicUsize::new(0)
);
static UTC_S_LAST_SET_ONDEMAND_CPU_OR_BETTER: once_cell::sync::Lazy<std::sync::atomic::AtomicUsize> = once_cell::sync::Lazy::new(||
  std::sync::atomic::AtomicUsize::new(0)
);


// Every state-capturing thing that _could_ want to put the CPU into high-performance or low-power mode
// has to call this thing, passing in any immediately-new info in an argument.
async fn make_cpu_governor_decisions(
  immediate_wanted_cpu_level: Option<&'static str>,
  cpu_load_vals: Option<(f64, f64, f64)>
) {
  let current_gov = get_cpu().await;
  let mut set_gov = current_gov;

  let forced_to_go_powersave = tokio::fs::metadata("/tmp/force-cpu-powersave").await.is_ok();
  let forced_to_go_performance = tokio::fs::metadata("/tmp/force-cpu-performance").await.is_ok();
  let user_override_exists = forced_to_go_powersave || forced_to_go_performance;

  if !user_override_exists {
    // Do what the window/event hints tell us is good to do
    if let Some(immediate_wanted_cpu_level) = immediate_wanted_cpu_level {
      if immediate_wanted_cpu_level != current_gov {
        set_cpu(immediate_wanted_cpu_level).await;
        set_gov = immediate_wanted_cpu_level;
      }
    }
    else {
      // Set up something based on historic data & current_gov

      let have_high_perf_bg_proc        = HAVE_HIGH_PERF_BG_PROC.load(std::sync::atomic::Ordering::SeqCst);
      let have_low_perf_window_visible  = HAVE_LOW_PERF_WINDOW_VISIBLE.load(std::sync::atomic::Ordering::SeqCst);
      let have_high_perf_window_visible = HAVE_HIGH_PERF_WINDOW_VISIBLE.load(std::sync::atomic::Ordering::SeqCst);

      let keyboard_is_6s_inactive = KEYBOARD_IS_6S_INACTIVE.load(std::sync::atomic::Ordering::SeqCst);
      let keyboard_is_30s_inactive = KEYBOARD_IS_30S_INACTIVE.load(std::sync::atomic::Ordering::SeqCst);

      if have_high_perf_bg_proc && current_gov != CPU_GOV_PERFORMANCE {
        set_cpu(CPU_GOV_PERFORMANCE).await;
        set_gov = CPU_GOV_PERFORMANCE;
      }
      else if have_high_perf_window_visible {

        if keyboard_is_6s_inactive && current_gov != CPU_GOV_CONSERVATIVE {
          set_cpu(CPU_GOV_CONSERVATIVE).await;
          set_gov = CPU_GOV_CONSERVATIVE;
        }
        else if keyboard_is_30s_inactive && current_gov != CPU_GOV_POWERSAVE {
          set_cpu(CPU_GOV_POWERSAVE).await;
          set_gov = CPU_GOV_POWERSAVE;
        }
        else if current_gov != CPU_GOV_PERFORMANCE {
          set_cpu(CPU_GOV_PERFORMANCE).await;
          set_gov = CPU_GOV_PERFORMANCE;
        }

      }
      else if have_low_perf_window_visible && current_gov != CPU_GOV_POWERSAVE {
        set_cpu(CPU_GOV_POWERSAVE).await;
        set_gov = CPU_GOV_POWERSAVE;
      }
      else {
        // Other system heuristics
        let focused_win_name = LAST_FOCUSED_WINDOW_NAME.read().await;
        let lower_focused_win_name = focused_win_name.to_lowercase();
        if current_gov == CPU_GOV_PERFORMANCE && !is_high_perf_window(&lower_focused_win_name) {
          // If we are at performance & there are NO high-performance windows, go to ondemand
          set_cpu(CPU_GOV_ONDEMAND).await;
          set_gov = CPU_GOV_ONDEMAND;
        }
        else if lower_focused_win_name.contains("mozilla firefox") && !(current_gov == CPU_GOV_ONDEMAND || current_gov == CPU_GOV_CONSERVATIVE) {
          set_cpu(CPU_GOV_ONDEMAND).await;
          set_gov = CPU_GOV_ONDEMAND;
        }

        // Only do this automatic load-based change if we are far away from having bumped CPU for performance wants
        let seconds_since_last_performance_wanted = (std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).expect("Time travel!").as_secs() as usize) - UTC_S_LAST_SET_PERFORMANCE_CPU.load(std::sync::atomic::Ordering::Relaxed);
        if seconds_since_last_performance_wanted > 32 {
          if let Some((one_m, five_m, fifteen_m)) = cpu_load_vals {
            // Test if system is under load & bump CPU?
            if one_m > 3.8 && five_m > 2.8 {
              // If > 3 cores are saturated, try to go to high performance
              if current_gov != CPU_GOV_PERFORMANCE {
                set_cpu(CPU_GOV_PERFORMANCE).await;
                set_gov = CPU_GOV_PERFORMANCE;
              }
            }
            else if one_m > 0.70 && one_m < 1.29 {
              // if ~1-2 core used go lower
              if current_gov != CPU_GOV_CONSERVATIVE {
                set_cpu(CPU_GOV_CONSERVATIVE).await;
                set_gov = CPU_GOV_CONSERVATIVE;
              }
            }
            else if one_m <= 0.70 {
              // if <1 core used go low
              if current_gov != CPU_GOV_POWERSAVE {
                set_cpu(CPU_GOV_POWERSAVE).await;
                set_gov = CPU_GOV_POWERSAVE;
              }
            }
          }
        }

        // end other system heuristics
      }
    }
  }
  else {
    // User override takes priority
    if forced_to_go_performance && current_gov != CPU_GOV_PERFORMANCE {
      set_cpu(CPU_GOV_PERFORMANCE).await;
      set_gov = CPU_GOV_PERFORMANCE;
    }
    else if forced_to_go_powersave && current_gov != CPU_GOV_POWERSAVE {
      set_cpu(CPU_GOV_POWERSAVE).await;
      set_gov = CPU_GOV_POWERSAVE;
    }
  }


  // Bookkeeping
  if set_gov == CPU_GOV_POWERSAVE {
    IN_POWERSAVE_MODE.store(true, std::sync::atomic::Ordering::Relaxed);
  }
  else {
    IN_POWERSAVE_MODE.store(false, std::sync::atomic::Ordering::Relaxed);
  }


  if set_gov == CPU_GOV_PERFORMANCE {
    UTC_S_LAST_SET_PERFORMANCE_CPU.store(
      std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).expect("Time travel!").as_secs() as usize,
      std::sync::atomic::Ordering::Relaxed
    );
    UTC_S_LAST_SET_ONDEMAND_CPU_OR_BETTER.store(
      std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).expect("Time travel!").as_secs() as usize,
      std::sync::atomic::Ordering::Relaxed
    );
  }
  else if set_gov == CPU_GOV_ONDEMAND {
    UTC_S_LAST_SET_ONDEMAND_CPU_OR_BETTER.store(
      std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).expect("Time travel!").as_secs() as usize,
      std::sync::atomic::Ordering::Relaxed
    );
  }

}


// anything < 4 is considered is an invalid value for PIDs,
// anything > 4 will be paused. If a request for a new PID to be
// paused comes in and all slots are full the request is ignored.
static PAUSED_PROC_PIDS: once_cell::sync::Lazy<[std::sync::atomic::AtomicI32; 32]> = once_cell::sync::Lazy::new(|| [
  std::sync::atomic::AtomicI32::new(0),
  std::sync::atomic::AtomicI32::new(0),
  std::sync::atomic::AtomicI32::new(0),
  std::sync::atomic::AtomicI32::new(0),
  std::sync::atomic::AtomicI32::new(0),
  std::sync::atomic::AtomicI32::new(0),
  std::sync::atomic::AtomicI32::new(0),
  std::sync::atomic::AtomicI32::new(0),

  std::sync::atomic::AtomicI32::new(0),
  std::sync::atomic::AtomicI32::new(0),
  std::sync::atomic::AtomicI32::new(0),
  std::sync::atomic::AtomicI32::new(0),
  std::sync::atomic::AtomicI32::new(0),
  std::sync::atomic::AtomicI32::new(0),
  std::sync::atomic::AtomicI32::new(0),
  std::sync::atomic::AtomicI32::new(0),

  std::sync::atomic::AtomicI32::new(0),
  std::sync::atomic::AtomicI32::new(0),
  std::sync::atomic::AtomicI32::new(0),
  std::sync::atomic::AtomicI32::new(0),
  std::sync::atomic::AtomicI32::new(0),
  std::sync::atomic::AtomicI32::new(0),
  std::sync::atomic::AtomicI32::new(0),
  std::sync::atomic::AtomicI32::new(0),

  std::sync::atomic::AtomicI32::new(0),
  std::sync::atomic::AtomicI32::new(0),
  std::sync::atomic::AtomicI32::new(0),
  std::sync::atomic::AtomicI32::new(0),
  std::sync::atomic::AtomicI32::new(0),
  std::sync::atomic::AtomicI32::new(0),
  std::sync::atomic::AtomicI32::new(0),
  std::sync::atomic::AtomicI32::new(0),
]);

async fn pause_proc(name: &str) {
  let mut paused_something = false;
  for p in dump_error_and_ret!( procfs::process::all_processes() ) {
    if let Ok(p) = p {
      if let Ok(p_exe) = p.exe() {
        if let Some(p_file_name) = p_exe.file_name() {
          let p_file_name = p_file_name.to_string_lossy();
          if p_file_name == name {
            if pause_pid( p.pid ).await {
              paused_something = true;
            }
          }
        }
      }
    }
  }
  if paused_something {
    notify(format!("paused {}", name).as_str()).await;
  }
}

async fn pause_pid(pid: i32) -> bool {

  // First check if we have already paused this PID + ignore if so
  for i in 0..PAUSED_PROC_PIDS.len() {
    if PAUSED_PROC_PIDS[i].load(std::sync::atomic::Ordering::Relaxed) == pid {
      return false;
    }
  }

  // Now write to first empty slot
  for i in 0..PAUSED_PROC_PIDS.len() {
    if PAUSED_PROC_PIDS[i].load(std::sync::atomic::Ordering::Relaxed) < 4 {
      PAUSED_PROC_PIDS[i].store(pid, std::sync::atomic::Ordering::Relaxed);
      dump_error!(
        nix::sys::signal::kill(
          nix::unistd::Pid::from_raw(pid), nix::sys::signal::Signal::SIGSTOP
        )
      );
      // Finally we move the SIGSTOP-ed process to the paused core
      let idle_core_num = PAUSED_PROC_CPU_CORE.load(std::sync::atomic::Ordering::SeqCst);
      let idle_core_num_s = format!("{}", idle_core_num);
      let pid_s = format!("{}", pid);
      dump_error!(
        tokio::process::Command::new("taskset")
          .args(&["--all-tasks", "-cp", &idle_core_num_s, &pid_s ])
          .status()
          .await
      );
      return true;
    }
  }

  return false;
}

// This is called on sigint / sigterm when we exit so we don't leave paused procs lying around
async fn unpause_all_paused_pids() {
  for i in 0..PAUSED_PROC_PIDS.len() {
    let pid = PAUSED_PROC_PIDS[i].load(std::sync::atomic::Ordering::Relaxed);
    if pid >= 4 {
      unpause_pid(pid).await;
    }
  }
}

async fn unmount_all_disks() {
  let mut all_possible_disk_mount_paths: Vec<String> = vec![];
  for (disk_block_device, disk_mount_items) in MOUNT_DISKS.entries() {
    for (disk_mount_path, disk_mount_opts) in disk_mount_items.iter() {
      all_possible_disk_mount_paths.push( disk_mount_path.to_string() );
    }
  }
  for (share_host, disk_mount_items) in MOUNT_NET_SHARES.entries() {
    for (disk_mount_path, disk_mount_opts) in disk_mount_items.iter() {
      all_possible_disk_mount_paths.push( disk_mount_path.to_string() );
    }
  }

  for _ in 0..4 { // just try several times in case a sub-directory is mounted?
    if let Ok(info) = mountinfo::MountInfo::new() {
      for possibly_mounted_disk_path in all_possible_disk_mount_paths.iter() {
        if is_mounted(&info, possibly_mounted_disk_path).await {
          dump_error!(
            tokio::process::Command::new("sudo")
              .args(&["-n", "umount", possibly_mounted_disk_path])
              .status()
              .await
          );
        }
      }
    }
  }

}

async fn unpause_pid(pid: i32) {
  // For all slots, set to 0 if == pid
  for i in 0..PAUSED_PROC_PIDS.len() {
    if PAUSED_PROC_PIDS[i].load(std::sync::atomic::Ordering::Relaxed) == pid {
      PAUSED_PROC_PIDS[i].store(0, std::sync::atomic::Ordering::Relaxed);
    }
  }
  // We first allow the PID to execute on _all_ cores (not just PAUSED_PROC_CPU_CORE)
  let pid_s = format!("{}", pid);
  dump_error!(
    tokio::process::Command::new("taskset")
      .args(&["--all-tasks", "-cp", "0-999", &pid_s ]) // TaskSet limits to max num CPUs, so 999 is safely "all cores" for the next 2 decades
      .status()
      .await
  );
  // Then SIGCONT
  dump_error_and_ret!(
    nix::sys::signal::kill(
      nix::unistd::Pid::from_raw(pid), nix::sys::signal::Signal::SIGCONT
    )
  );
}


async fn unpause_proc(name: &str) {
  let mut un_paused_something = false;
  for p in dump_error_and_ret!( procfs::process::all_processes() ) {
    if let Ok(p) = p {
      if let Ok(p_exe) = p.exe() {
        if let Some(p_file_name) = p_exe.file_name() {
          let p_file_name = p_file_name.to_string_lossy();
          if p_file_name == name {
            unpause_pid( p.pid ).await;
            un_paused_something = true;
          }
        }
      }
    }
  }
  if un_paused_something {
    notify(format!("un-paused {}", name).as_str()).await;
  }
}

async fn partial_resume_paused_procs() {
  let mut interval = tokio::time::interval(tokio::time::Duration::from_millis(600));
  interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
  let mut powersave_interval = tokio::time::interval(tokio::time::Duration::from_millis(1300));
  powersave_interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
  loop {
    let in_powersave_mode = IN_POWERSAVE_MODE.load(std::sync::atomic::Ordering::Relaxed);
    if in_powersave_mode {
      powersave_interval.tick().await;
    }
    else {
      interval.tick().await;
    }

    for i in 0..PAUSED_PROC_PIDS.len() {
      let proc_pid = PAUSED_PROC_PIDS[i].load(std::sync::atomic::Ordering::Relaxed);
      if proc_pid >= 4 {
        // First see if no longer running + zero if so
        if let Err(_e) = procfs::process::Process::new(proc_pid) {
          PAUSED_PROC_PIDS[i].store(0, std::sync::atomic::Ordering::Relaxed);
        }
        else {
          // PID is running and was stopped, continue it!
          dump_error!(
            nix::sys::signal::kill(
              nix::unistd::Pid::from_raw(proc_pid), nix::sys::signal::Signal::SIGCONT
            )
          );
        }
      }
    }

    // Delay for 0.2s to allow continued procs to run
    if in_powersave_mode {
      tokio::time::sleep( std::time::Duration::from_millis(115) ).await;
    }
    else {
      tokio::time::sleep( std::time::Duration::from_millis(85) ).await;
    }

    for i in 0..PAUSED_PROC_PIDS.len() {
      let proc_pid = PAUSED_PROC_PIDS[i].load(std::sync::atomic::Ordering::Relaxed);
      if proc_pid >= 4 {
        // Go back to sleep
        dump_error!(
          nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(proc_pid), nix::sys::signal::Signal::SIGSTOP
          )
        );
      }
    }

  }
}


async fn mount_swap_files() {
  let mut interval = tokio::time::interval(tokio::time::Duration::from_millis(6800));
  interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
  let mut powersave_interval = tokio::time::interval(tokio::time::Duration::from_millis(18600));
  powersave_interval.set_missed_tick_behavior(MissedTickBehavior::Delay);

  interval.tick().await;

  let external_scratch_usb_disk = "/dev/disk/by-partuuid/e08214f5-cfc5-4252-afee-505dfcd23808";
  let internal_sd_card = "/dev/disk/by-partuuid/8f3ca68c-d031-2d41-849c-be5d9602e920";

  let mount_info = dump_error_and_ret!( mountinfo::MountInfo::new() );

  let swap_file_dir: std::path::PathBuf = match get_mount_pt_of(&mount_info, external_scratch_usb_disk).await {
    Some(mount_pt) => {
      mount_pt.join("swap-files")
    }
    None => {
      match get_mount_pt_of(&mount_info, internal_sd_card).await {
        Some(mount_pt) => {
          mount_pt.join("swap-files")
        }
        None => {
          // no swap-files parent dir exists; if we return, we will be _immediately_ re-started.
          // Instead we just delay for 24 hours.
          tokio::time::sleep( tokio::time::Duration::from_secs(24 * 60 * 60) ).await;
          return;
        }
      }
    }
  };

  if ! swap_file_dir.exists() {
    dump_error!( tokio::fs::create_dir_all(&swap_file_dir).await );
  }

  // Each tick, if memory use including swap is above 80%, we add 4gb of swap.
  // If memory use including swap is under 60%, we remove swap until this is 0.
  // Swap files are named {swap_file_dir}/swap-{num_swap_files_added}
  let mut num_swap_files_added = 0;

  loop {
    if IN_POWERSAVE_MODE.load(std::sync::atomic::Ordering::Relaxed) {
      powersave_interval.tick().await;
    }
    else {
      interval.tick().await;
    }

    if ! swap_file_dir.exists() {
      break; // It could happen, so re-start future & pick up the other card.
    }

    // See https://docs.rs/nix/latest/nix/sys/sysinfo/struct.SysInfo.html
    //let info = nix::sys::sysinfo();
    if let Ok(info) = nix::sys::sysinfo::sysinfo() {

      let total_swap_bytes = info.swap_total();
      let free_swap_bytes = info.swap_free();
      let used_swap_bytes = total_swap_bytes - free_swap_bytes;

      let swap_fraction_used: f64 = used_swap_bytes as f64 / total_swap_bytes as f64;
      if swap_fraction_used > 0.40 && num_swap_files_added < 20 /* at 80gb of swap we have other problems */ {
        // Add more!
        num_swap_files_added += 1;
        let next_swap_file = (&swap_file_dir).join(format!("swap-{}", num_swap_files_added));
        if ! next_swap_file.exists() {
          dump_error!(
            tokio::process::Command::new("sudo")
              .args(&["-n", "truncate", "-s", "0", &next_swap_file.to_string_lossy() ])
              .status()
              .await
          );

          dump_error!(
            tokio::process::Command::new("sudo")
              .args(&["-n", "chmod", "600", &next_swap_file.to_string_lossy() ])
              .status()
              .await
          );

          dump_error!( // Disable copy-on-write
            tokio::process::Command::new("sudo")
              .args(&["-n", "chattr", "+C", &next_swap_file.to_string_lossy() ])
              .status()
              .await
          );

          dump_error!(
            tokio::process::Command::new("sudo")
              .args(&["-n", "fallocate", "-l", "4G", &next_swap_file.to_string_lossy() ])
              .status()
              .await
          );

          dump_error!(
            tokio::process::Command::new("sudo")
              .args(&["-n", "mkswap", &next_swap_file.to_string_lossy() ])
              .status()
              .await
          );
        }

        dump_error!(
          tokio::process::Command::new("sudo")
            .args(&["-n", "swapon", &next_swap_file.to_string_lossy() ])
            .status()
            .await
        );


      }
      else if swap_fraction_used < 0.30 && num_swap_files_added > 0 {
        // Remove some!
        let last_swap_file = swap_file_dir.join(format!("swap-{}", num_swap_files_added));
        if last_swap_file.exists() {
          // Swapoff
          dump_error!(
            tokio::process::Command::new("sudo")
              .args(&["-n", "swapoff", &last_swap_file.to_string_lossy() ])
              .status()
              .await
          );
        }

      }

      let (one_m, five_m, fifteen_m) = info.load_average();
      make_cpu_governor_decisions(None, Some((one_m, five_m, fifteen_m)) ).await;

    }

  }
}



async fn turn_off_misc_lights() {
  let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(128));
  interval.set_missed_tick_behavior(MissedTickBehavior::Delay);

  // find /sys -name brightness -print -exec cat {} \; 2>/dev/null
  let files_to_write_0_to = &[
    "/sys/devices/platform/thinkpad_acpi/leds/platform::micmute/brightness",
    "/sys/devices/platform/thinkpad_acpi/leds/tpacpi::power/brightness",
    "/sys/devices/platform/thinkpad_acpi/leds/tpacpi::lid_logo_dot/brightness",
    "/sys/devices/platform/thinkpad_acpi/leds/tpacpi::thinkvantage/brightness",
    "/sys/devices/platform/thinkpad_acpi/leds/platform::mute/brightness",
  ];

  loop {
    interval.tick().await;

    for file_to_write_0_to in files_to_write_0_to.iter() {
      if std::path::Path::new(file_to_write_0_to).exists() {
        // If the current value is not 0, write 0 to the file
        let mut write_0_needed = true;
        if let Ok(contents) = tokio::fs::read_to_string(file_to_write_0_to).await {
          if contents.starts_with("0") {
            write_0_needed = false;
          }
        }
        if write_0_needed {
          util::write_to_sysfs_file(file_to_write_0_to, "0\n").await;
        }
      }
    }

  }
}

async fn update_dns_records() {
  // /j/bins/update-dns.py only performs network I/O if the IP values have changed
  // since last run, so we can run this more often to get better time granularity w/o
  // thrashing our DNS provider
  let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(180));
  interval.set_missed_tick_behavior(MissedTickBehavior::Delay);

  loop {
    interval.tick().await;

    if LID_IS_CLOSED.load(std::sync::atomic::Ordering::Relaxed) {
      continue; // No network ops when lid closed!
    }

    dump_error_and_ret!(
      tokio::process::Command::new("/j/bin/update-dns")
        .status()
        .await
    );

  }
}

async fn run_disregarded_pvms() {
  let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(300));
  interval.set_missed_tick_behavior(MissedTickBehavior::Delay);

  loop {
    interval.tick().await;

    if LID_IS_CLOSED.load(std::sync::atomic::Ordering::Relaxed) {
      continue; // No VM ops when lid closed!
    }

    dump_error_and_ret!(
      tokio::process::Command::new("/j/bins/windows-vm-bg-runner.sh")
        .status()
        .await
    );

  }
}

async fn run_package_downloader() {
  // Check for new packages every 2 hours
  // Pacman already uses a lock to ensure we cannot run while another instance is running
  let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(2 * 60 * 60));
  interval.set_missed_tick_behavior(MissedTickBehavior::Delay);

  // Run early
  tokio::time::sleep( tokio::time::Duration::from_millis(2500) ).await;
  dump_error_and_ret!(
    tokio::process::Command::new("/j/bins/update_prefetcher.py")
      .status()
      .await
  );

  loop {
    interval.tick().await;

    if LID_IS_CLOSED.load(std::sync::atomic::Ordering::Relaxed) {
      continue; // No package downloads when lid closed!
    }

    dump_error_and_cont!(
      tokio::process::Command::new("/j/bins/update_prefetcher.py")
        .status()
        .await
    );

  }
}

async fn log_runtime_stats() {
    const RUNTIME_STATS_FILE: &'static str = "/tmp/eventmgr-runtime-stats";

    use tokio::fs::OpenOptions;
    use tokio::io::AsyncWriteExt;
    use std::io::Write;

    let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(48));
    interval.set_missed_tick_behavior(MissedTickBehavior::Delay);

    loop {
      interval.tick().await;

      if let Ok(mut f) = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(RUNTIME_STATS_FILE)
        .await
      {

        let mut buf = Vec::<u8>::new();

        if let Ok(agg) = AGG.lock() { // NB: Cannot hold agg across .await points.
          let mut names_in_order_duration: Vec<String> = vec![];
          let mut longest_name_chars = 0;
          for (name, dur) in agg.iter() {
            names_in_order_duration.push(name.to_string());
            if name.len() > longest_name_chars {
              longest_name_chars = name.len();
            }
          }

          names_in_order_duration.sort_by_key(|k| agg.get(k.as_str()) );

          for name in names_in_order_duration.iter() {
            if let Some(dur) = agg.get(name.as_str()) {
              writeln!(buf, "{: <width$}  {:?}", name, dur, width = longest_name_chars).unwrap();
            }
          }
        }

        dump_error!( f.write_all(&buf).await );

        println!("Logged CPU times to {}", RUNTIME_STATS_FILE);
      }


    }
}


pub const PERIODIC_RTC_WAKE_SECONDS: u64 = 25 * 60; // 25 minute wakeup, poll things, then back to sleep.
//pub const PERIODIC_RTC_WAKE_SECONDS: u64 = 25;

async fn handle_lid_states() {

  const ACPI_PATH: &'static str = "/proc/acpi/button/lid/LID/state";

  let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(2));
  interval.set_missed_tick_behavior(MissedTickBehavior::Delay);

  let acpi_path = std::path::PathBuf::from(ACPI_PATH);

  if ! acpi_path.exists() {
    println!("Fatal error in handle_lid_states: {} does not exist!", ACPI_PATH);
    tokio::time::sleep( tokio::time::Duration::from_millis(500) ).await;
    return;
  }

  let mut lid_was_closed = false;

  let mut loop_n = 0;

  loop {
    interval.tick().await;
    loop_n += 1;
    if loop_n > 1000000 {
      loop_n = 0;
    }

    if util::is_lid_closed(ACPI_PATH).await {
      if ! lid_was_closed {
        // Do on-lid-close tasks
        on_lid_close().await;
      }
      lid_was_closed = true;
      if loop_n % 8 == 0 {
        periodic_while_lid_closed().await;
      }
    }
    else {
      if lid_was_closed {
        on_lid_open().await;
      }
      lid_was_closed = false;
    }
  }
}

async fn on_lid_close() {
  LID_IS_CLOSED.store(false, std::sync::atomic::Ordering::SeqCst);
  /*tokio::task::spawn(
    util::blink_lid_thinkpad_led(&[
      true, true, false,
      true, true, false,
      true, true, false,
      true, true, false,
      true, true, false,
      true, true, false,
      true, true, false,
    ])
  );*/
  make_cpu_governor_decisions(Some(CPU_GOV_POWERSAVE), None).await;
  dump_error!(
    tokio::process::Command::new("sudo")
      .args(&["-n", "/j/bins/super-affinity.sh", "ecores" ])
      .status()
      .await
  );

  go_to_sleep_with_wakeup_after(PERIODIC_RTC_WAKE_SECONDS).await;

}

async fn go_to_sleep_with_wakeup_after(s: u64) {
  // Schedule a system wake up s from now
  tokio::time::sleep( tokio::time::Duration::from_millis(50) ).await; // delay makes RTC timer more reliable!
  let wakeup_time = std::time::SystemTime::now() + tokio::time::Duration::from_secs(s);
  let wakeup_ts = wakeup_time
        .duration_since(std::time::UNIX_EPOCH)
        .expect("Time went backwards")
        .as_secs();

  util::write_to_sysfs_file("/sys/class/rtc/rtc0/wakealarm", "0").await;
  tokio::time::sleep( tokio::time::Duration::from_millis(50) ).await; // delay makes RTC timer more reliable!
  util::write_to_sysfs_file("/sys/class/rtc/rtc0/wakealarm", format!("{}", wakeup_ts).as_str()).await;

  tokio::time::sleep( tokio::time::Duration::from_millis(350) ).await;

  // Go to RAM sleep
  util::write_to_sysfs_file("/sys/power/state", "mem").await;
}

async fn periodic_while_lid_closed() {
  // Tell user we're alive
  util::blink_lid_thinkpad_led(&[
    true, true, false,
    true, true, false,
  ]).await;

  // Any checks to do?


  // Go back to sleep
  go_to_sleep_with_wakeup_after(PERIODIC_RTC_WAKE_SECONDS).await;
}

async fn on_lid_open() {
  LID_IS_CLOSED.store(false, std::sync::atomic::Ordering::SeqCst);
  dump_error!(
    tokio::process::Command::new("sudo")
      .args(&["-n", "/j/bins/super-affinity.sh", "all" ])
      .status()
      .await
  );
  tokio::task::spawn(
    util::blink_power_thinkpad_led(&[
      true, false, false, false,
      true, false, false, false,
      true, false, false, false,
      true, false, false, false,
      true, false, false, false,
    ])
  );
}





