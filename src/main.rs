
use futures::prelude::*;

use std::borrow::Borrow;



pub const SERVER_SOCKET: &'static str = "/tmp/eventmgr.sock";

pub mod structs;
#[macro_use]
pub mod macros;

pub mod client;

use structs::*;
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


async fn eventmgr() {
  notify("Beginning eventmgr event loop...").await;

  let mut tasks = vec![
    PersistentAsyncTask::new("handle_exit_signals",              ||{ tokio::task::spawn(handle_exit_signals()) }),
    PersistentAsyncTask::new("handle_sway_msgs",                 ||{ tokio::task::spawn(handle_sway_msgs()) }),
    PersistentAsyncTask::new("poll_device_audio_playback",       ||{ tokio::task::spawn(poll_device_audio_playback()) }),
    PersistentAsyncTask::new("handle_socket_msgs",               ||{ tokio::task::spawn(handle_socket_msgs()) }),
    PersistentAsyncTask::new("poll_downloads",                   ||{ tokio::task::spawn(poll_downloads()) }),
    PersistentAsyncTask::new("poll_ff_bookmarks",                ||{ tokio::task::spawn(poll_ff_bookmarks()) }),
    PersistentAsyncTask::new("poll_wallpaper_rotation",          ||{ tokio::task::spawn(poll_wallpaper_rotation()) }),
    PersistentAsyncTask::new("poll_check_dexcom",                ||{ tokio::task::spawn(poll_check_dexcom()) }),
    PersistentAsyncTask::new("mount_disks",                      ||{ tokio::task::spawn(mount_disks()) }),
    PersistentAsyncTask::new("bump_cpu_for_performance_procs",   ||{ tokio::task::spawn(bump_cpu_for_performance_procs()) }),
    PersistentAsyncTask::new("partial_resume_paused_procs",      ||{ tokio::task::spawn(partial_resume_paused_procs()) }),
    PersistentAsyncTask::new("bind_mount_azure_data",            ||{ tokio::task::spawn(bind_mount_azure_data()) }),
    PersistentAsyncTask::new("mount_swap_files",                 ||{ tokio::task::spawn(mount_swap_files()) }),
  ];

  // We check for failed tasks and re-start them every 6 seconds
  let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(6));
  
  loop {
    interval.tick().await;

    for i in 0..tasks.len() {
      tasks[i].ensure_running().await;
    }

  }
}

async fn notify(msg: &str) {
  println!("{}", msg);
  dump_error!(
    notify_rust::Notification::new()
      .summary("EventMgr")
      .body(msg)
      //.icon("firefox")
      .timeout(notify_rust::Timeout::Milliseconds(6400)) //milliseconds
      .show()
  );
}

fn notify_sync(msg: &str) {
  println!("{}", msg);
  dump_error!(
    notify_rust::Notification::new()
      .summary("EventMgr")
      .body(msg)
      //.icon("firefox")
      .timeout(notify_rust::Timeout::Milliseconds(6400)) //milliseconds
      .show()
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
    let mut want_shutdown = false;
    tokio::select!{
      _sig_int = int_stream.recv() => { want_shutdown = true; }
      _sig_term = term_stream.recv() => { want_shutdown = true; }
    };
    if want_shutdown {
      println!("Got SIG{{TERM/INT}}, shutting down!");
      unpause_all_paused_pids().await;
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
            let name = window_evt.container.name.clone().unwrap_or("".to_string());
            on_window_focus(&name, &window_evt.container).await;
          }
        }
        swayipc_async::Event::Workspace(workspace_evt) => {
          if workspace_evt.change == swayipc_async::WorkspaceChange::Focus {
            if let Some(focused_ws) = workspace_evt.current {
              let name = focused_ws.name.unwrap_or("".to_string());
              on_workspace_focus(&name).await;
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
  if let Ok(mut conn) = swayipc_async::Connection::new().await {
    for output in dump_error_and_ret!( conn.get_outputs().await ) {
      let wallpaper_cmd = format!("output {} bg {} fill", output.name, wallpaper);
      println!("Running wallpaper cmd: {}", wallpaper_cmd);
      dump_error_and_ret!( conn.run_command(wallpaper_cmd).await );
    }
  }
}


static UTC_S_LAST_SEEN_FS_TEAM_FORTRESS: once_cell::sync::Lazy<std::sync::atomic::AtomicUsize> = once_cell::sync::Lazy::new(||
  std::sync::atomic::AtomicUsize::new(0)
);

static LAST_FOCUSED_WINDOW_NAME: once_cell::sync::Lazy<tokio::sync::RwLock<String>> = once_cell::sync::Lazy::new(||
  tokio::sync::RwLock::new( "".to_owned() )
);

async fn on_window_focus(window_name: &str, sway_node: &swayipc_async::Node) {
  println!("Window focused = {:?}", window_name );

  {
    let mut last_focused_window_name = LAST_FOCUSED_WINDOW_NAME.write().await;
    *last_focused_window_name = window_name.to_owned();
  }

  darken_kbd_if_video_focused_and_audio_playing().await;

  let lower_window = window_name.to_lowercase();
  if lower_window.contains("team fortress") && lower_window.contains("opengl") {
    on_wanted_cpu_level(CPU_GOV_PERFORMANCE).await;
    unpause_proc("hl2_linux").await;
    UTC_S_LAST_SEEN_FS_TEAM_FORTRESS.store(
      std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).expect("Time travel!").as_secs() as usize,
      std::sync::atomic::Ordering::Relaxed
    );
  }
  else {
    if lower_window.contains("mozilla firefox") {
      on_wanted_cpu_level(CPU_GOV_ONDEMAND).await; // todo possibly a custom per-browser ask
    }
    else {
      on_wanted_cpu_level(CPU_GOV_ONDEMAND).await;
    }

    // Only pause IF we've seen team fortress fullscreen in the last 15 minutes / 900s
    let seconds_since_saw_tf2_fs = (std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).expect("Time travel!").as_secs() as usize) - UTC_S_LAST_SEEN_FS_TEAM_FORTRESS.load(std::sync::atomic::Ordering::Relaxed);
    if seconds_since_saw_tf2_fs < 900 {
      pause_proc("hl2_linux").await;
    }

  }


}

static CURRENT_KBD_LIGHT_VAL: once_cell::sync::Lazy<std::sync::atomic::AtomicU32> = once_cell::sync::Lazy::new(||
  std::sync::atomic::AtomicU32::new(5)
);

async fn darken_kbd_if_video_focused_and_audio_playing() {
  let currently_playing_audio = CURRENTLY_PLAYING_AUDIO.load(std::sync::atomic::Ordering::SeqCst);
  if ! currently_playing_audio {
    set_kbd_light(1).await;
    return;
  }
  // We are playing audio!
  let focused_win_name = LAST_FOCUSED_WINDOW_NAME.read().await;
  let focused_win_name = focused_win_name.to_lowercase();

  let is_video = focused_win_name.contains("youtube") ||
                 focused_win_name.contains("mpv") ||
                 focused_win_name.contains("youtube");

  if is_video {
    set_kbd_light(0).await;
  }
  else {
    set_kbd_light(1).await;
  }

}

async fn set_kbd_light(level: u32) {
  let current_level = CURRENT_KBD_LIGHT_VAL.load(std::sync::atomic::Ordering::Relaxed);
  if current_level == level {
    return;
  }

  if let Ok(led_devices) = bulbb::misc::LedDevice::get_all_led_devices() {
    for ld in led_devices {
      if let Some(bulbb::misc::LedFunction::KbdBacklight) = ld.info.function {
        dump_error!( ld.set_brightness(level) );
      }
    }
  }

  CURRENT_KBD_LIGHT_VAL.store(level, std::sync::atomic::Ordering::Relaxed);

}

static CURRENTLY_PLAYING_AUDIO: once_cell::sync::Lazy<std::sync::atomic::AtomicBool> = once_cell::sync::Lazy::new(||
  std::sync::atomic::AtomicBool::new(false)
);

async fn poll_device_audio_playback() {
  use alsa::pcm::*;
  use alsa::{Direction, ValueOr, Error};

  // Ensure pulse can connect!
  if ! std::env::var("DBUS_SESSION_BUS_ADDRESS").is_ok() {
    std::env::set_var("DBUS_SESSION_BUS_ADDRESS", "unix:path=/run/user/1000/bus");
  }

  if ! std::env::var("XDG_RUNTIME_DIR").is_ok() { // Just guessing here
    std::env::set_var("XDG_RUNTIME_DIR", "/run/user/1000");
  }


  // Calculates RMS (root mean square) as a way to determine volume
  fn rms(buf: &[i16]) -> f64 {
      if buf.len() == 0 { return 0f64; }
      let mut sum = 0f64;
      for &x in buf {
          sum += (x as f64) * (x as f64);
      }
      let r = (sum / (buf.len() as f64)).sqrt();
      // Convert value to decibels
      20.0 * (r / (i16::MAX as f64)).log10()
  }
  
  let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(3));

  interval.tick().await; // Wait 1 tick before recording

  loop {
    interval.tick().await;

    dump_error_and_ret!( tokio::task::spawn_blocking(move || {
      // New connection each poll, why not?
      let pcm = dump_error_and_ret!( PCM::new("default", Direction::Capture, false) );
      {
          // For this example, we assume 44100Hz, one channel, 16 bit audio.
          let hwp = dump_error_and_ret!( HwParams::any(&pcm) );
          dump_error_and_ret!( hwp.set_channels(1) );
          dump_error_and_ret!( hwp.set_rate(44100, ValueOr::Nearest) );
          dump_error_and_ret!( hwp.set_format(Format::s16()) );
          dump_error_and_ret!( hwp.set_access(Access::RWInterleaved) );
          dump_error_and_ret!( pcm.hw_params(&hwp) );
      }
      dump_error_and_ret!( pcm.start() );

      let io_stream = dump_error_and_ret!( pcm.io_i16() );
      let mut sound_buffer = [0i16; 8192];

      let num_read_bytes = dump_error_and_ret!( io_stream.readi(&mut sound_buffer) );
      let audio_vol_amount = rms(&sound_buffer[0..num_read_bytes]);

      //println!("audio_vol_amount = {}", audio_vol_amount);

      if audio_vol_amount < -500.0 { // "regular" numbers are around -25.0 or so, so significantly below this (incl -inf) is no audio!
        CURRENTLY_PLAYING_AUDIO.store(false, std::sync::atomic::Ordering::SeqCst);
      }
      else {
        CURRENTLY_PLAYING_AUDIO.store(true, std::sync::atomic::Ordering::SeqCst);
      }
    }).await );

    darken_kbd_if_video_focused_and_audio_playing().await;

  }

}


async fn on_workspace_focus(workspace_name: &str) {
  println!("Workspace focused = {:?}", workspace_name );
}

// static LAST_CPU_LEVEL: once_cell::sync::Lazy<(&str, usize)> = once_cell::sync::Lazy::new(|| (CPU_GOV_ONDEMAND, 0) );

async fn on_wanted_cpu_level(wanted_cpu_level: &str) {
  let current_cpu_level = get_cpu().await;
  if current_cpu_level == wanted_cpu_level {
    return; // NOP
  }
  set_cpu(wanted_cpu_level).await;
}

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
          println!("Got message: {:?}", &msg_val );
          // TODO handle message
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


pub const DOWNLOAD_QUARANTINE_SECS: u64 = (24 + 12) * (60 * 60);
pub const QUARANTINE_DELETE_SECS: u64 = (3 * 24) * (60 * 60);

async fn poll_downloads() {
  let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(60));
  loop {
    interval.tick().await;

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
              dump_error_and_ret!( tokio::fs::rename(entry.path(), dst_file).await );
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

        }
      }
    }



  }
}


async fn poll_ff_bookmarks() {
  let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(60));
  loop {
    interval.tick().await;

    // See https://crates.io/crates/marionette

    
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
  let mut interval = tokio::time::interval(tokio::time::Duration::from_secs( 180 ));

  let mut weights_total: usize = 0;
  for (_, weight) in WALLPAPER_DIR_WEIGHTS.entries() {
    weights_total += weight;
  }

  loop {
    interval.tick().await;

    let mut picked_wp_dir = "";

    for _ in 0..500 { // "kinda" an infinite loop, exiting when a dir has been picked
      for (wp_dir, dir_weight) in WALLPAPER_DIR_WEIGHTS.entries() {
        let rand_num = fastrand::usize(0..weights_total);
        if rand_num <= *dir_weight {
          picked_wp_dir = wp_dir;
          break;
        }
      }
      if picked_wp_dir.len() > 0 {
        break;
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

async fn poll_check_dexcom() {
  let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(120));
  loop {
    interval.tick().await;

    // dexcom /tmp/dexcom.txt
    dump_error_and_ret!(
      tokio::process::Command::new("/j/bin/dexcom")
        .args(&["/tmp/dexcom.txt"])
        .status()
        .await
    );
    
  }
}




async fn bind_mount_azure_data() {
  let azure_data_block_dev = std::path::Path::new("/dev/disk/by-partuuid/8f3ca68c-d031-2d41-849c-be5d9602e920");
  let azure_data_mount = std::path::Path::new("/mnt/azure-data");
  let tmp_data_mount = std::path::Path::new("/tmp");

  let data_mount_points = &[
    // root FS path, relative to azure_data_mount path
    ("/var/cache/pacman/pkg",     "azure_sys/var_cache_pacman_pkg"),
    ("/j/.cache/mozilla/firefox", "azure_sys/j_.cache_mozilla_firefox"), // The only folder in here should be jeff_2023, corresponding to the PROFILE under .mozilla which must never be removed.
    ("/j/downloads",              "azure_sys/j_downloads"),

  ];

  let build_dir_scan_folders = &[
    "/j/proj", // TODO
  ];

  // n == neither, d == data mounted, t == tmpfs mounted
  let mut data_mount_points_mounted = 'n';

  let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(6));

  loop {
    interval.tick().await; // wait at least one tick so we can fail early below w/o a hugely infinite loop
    if azure_data_block_dev.exists() && is_mounted( &azure_data_mount.to_string_lossy() ).await {
      break;
    }
  }

  // Ensure ownership of azure_data_mount
  dump_error_and_ret!(
    tokio::process::Command::new("sudo")
      .args(&["-n", "chown", "jeffrey", &azure_data_mount.to_string_lossy() ])
      .status()
      .await
  );

  // Firstly; iterate all system directories and delete child contents
  for (root_fs_dir, data_mnt_path) in data_mount_points.iter() {
    // If root_fs_dir is mounted, unmount it.
    if is_mounted(root_fs_dir).await {
      dump_error_and_ret!(
        tokio::process::Command::new("sudo")
          .args(&["-n", "umount", root_fs_dir])
          .status()
          .await
      );
    }

    // Also create data_mnt_path if ! exists
    let data_mnt_path = azure_data_mount.join(data_mnt_path);
    if ! data_mnt_path.exists() {
      // Create it!
      dump_error_and_ret!(
        tokio::fs::create_dir_all(&data_mnt_path).await
      );
    }

    // Now is it mounted?
    if is_mounted(root_fs_dir).await {
      continue; // Do not rm -rf files if they are mounted, just ignore for now.
    }

    // After un-mounting, delete everything! This is a cache folder!
    let mut root_fs_dir_o = dump_error_and_ret!( tokio::fs::read_dir(root_fs_dir).await );
    while let Some(child) = dump_error_and_ret!( root_fs_dir_o.next_entry().await ) {
      if child.file_name() == "." || child.file_name() == ".." { // Jeff this doesn't happen and you know it -_-
        continue;
      }
      // Remove it!
      if child.path().is_dir() {
        dump_error!( std::fs::remove_dir_all( child.path() ) );
      }
      else {
        dump_error!( std::fs::remove_file( child.path() ) );
      }
    }

  }

  loop {
    interval.tick().await;
    
    // If the block device exists & we have not mounted to it, do that.
    if azure_data_block_dev.exists() && azure_data_mount.exists() && data_mount_points_mounted != 'd' {
      // bind-mount all folders in data_mount_points
      for (root_fs_dir, data_mnt_path) in data_mount_points.iter() {
        let data_mnt_path = azure_data_mount.join(data_mnt_path);
        dump_error!(
          tokio::process::Command::new("sudo")
            .args(&["-n", "mount", "--bind", &data_mnt_path.to_string_lossy(), root_fs_dir ])
            .status()
            .await
        );
      }

      data_mount_points_mounted = 'd';
      notify(format!("Mounted {} system caches to azure-data", data_mount_points.len() ).as_str()).await;
    }
    else if azure_data_block_dev.exists() && !azure_data_mount.exists() {
      // We are waiting for other future to mount disk, do nothing.
    }
    else if !azure_data_block_dev.exists() && data_mount_points_mounted != 't' {
      // Disk was unplugged! Un-mount the bind mounts and replace w/ tmpfs mounts
      for (root_fs_dir, _data_mnt_path) in data_mount_points.iter() {
        dump_error!(
          tokio::process::Command::new("sudo")
            .args(&["-n", "umount", root_fs_dir])
            .status()
            .await
        );
      }

      // Bind-mount our /tmp/ to the folders to avoid data falling on to the root FS
      for (root_fs_dir, data_mnt_path) in data_mount_points.iter() {
        let data_mnt_path = tmp_data_mount.join(data_mnt_path);
        dump_error!(
          tokio::process::Command::new("sudo")
            .args(&["-n", "mount", "--bind", &data_mnt_path.to_string_lossy(), root_fs_dir  ])
            .status()
            .await
        );
      }

      data_mount_points_mounted = 't';
      notify(format!("Mounted {} system caches to TMPFS", data_mount_points.len() ).as_str()).await;
    }

  }
}



static MOUNT_DISKS: phf::Map<&'static str, &[(&'static str, &'static str)] > = phf::phf_map! {
  "/dev/disk/by-partuuid/8f3ca68c-d031-2d41-849c-be5d9602e920" => 
    &[
      ("/mnt/azure-data", "defaults,rw,autodefrag,compress=zstd:11,commit=300,nodatasum"),
      // sudo rmdir /mnt/azure-data/swap-files ; sudo btrfs subvolume create '/mnt/azure-data/@swap-files' ; sudo btrfs property set /mnt/azure-data/swap-files compression none
      ("/mnt/azure-data/swap-files", "defaults,rw,noatime,nodatacow,subvol=@swap-files,nodatasum")
    ],
    // ^^ compress=lzo is fast w/o huge compression ratios, zstd:9 is slower with better ratio. high commit seconds means disk writes are less common.

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
  "/sys/class/power_supply/apple_mfi_fastcharge" =>
    &[
      ("/mnt/iphone-root", "ifuse -o allow_other,rw /mnt/iphone-root"),
      ("/mnt/iphone-vox",  "ifuse --documents com.coppertino.VoxMobile -o allow_other,rw /mnt/iphone-vox")
    ],

};

async fn mount_disks() {
  let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(4));

  loop {
    interval.tick().await;

    for (disk_block_device, disk_mount_items) in MOUNT_DISKS.entries() {
      for (disk_mount_path, disk_mount_opts) in disk_mount_items.iter() {
        if std::path::Path::new(disk_block_device).exists() {
          if ! is_mounted(disk_mount_path).await {
            if ! std::path::Path::new(disk_mount_path).exists() {
              // Sudo create it
              dump_error_and_ret!(
                tokio::process::Command::new("sudo")
                  .args(&["-n", "mkdir", "-p", disk_mount_path])
                  .status()
                  .await
              );
            }

            // Even if path previously exists, ensure jeffrey owns it
            dump_error_and_ret!(
              tokio::process::Command::new("sudo")
                .args(&["-n", "chown", "jeffrey", disk_mount_path])
                .status()
                .await
            );

            // If there are space chars in disk_mount_opts run as a command, else pass to "mount"
            if disk_mount_opts.contains(" ") {
              dump_error_and_ret!(
                tokio::process::Command::new("sudo")
                  .args(&["-n", "sh", "-c", disk_mount_opts])
                  .status()
                  .await
              );
            }
            else {
              dump_error_and_ret!(
                tokio::process::Command::new("sudo")
                  .args(&["-n", "mount", "-o", disk_mount_opts, disk_block_device, disk_mount_path])
                  .status()
                  .await
              );
            }

          }
        }
        else {
          // Block device does NOT exist, remove mountpoint if it exists!
          if std::path::Path::new(disk_mount_path).exists() {
            dump_error!(
              tokio::process::Command::new("sudo")
                .args(&["-n", "umount", disk_mount_path])
                .status()
                .await
            );
            dump_error!(
              tokio::process::Command::new("sudo")
                .args(&["-n", "rmdir", disk_mount_path])
                .status()
                .await
            );
          }
        }
      }
    }
  }
}



async fn bump_cpu_for_performance_procs() {
  let mut interval = tokio::time::interval(tokio::time::Duration::from_millis(1400));

  let mut have_high_cpu = false;
  loop {
    interval.tick().await;

    let mut want_high_cpu = false;
    for p in dump_error_and_ret!( procfs::process::all_processes() ) {
      if let Ok(p) = p {
        if let Ok(p_exe) = p.exe() {
          if let Some(p_file_name) = p_exe.file_name() {
            let p_file_name = p_file_name.to_string_lossy();
            let heavy_p_running =
              p_file_name == "gcc" || p_file_name == "clang" ||
              p_file_name == "g++" || p_file_name == "clang++" ||
              p_file_name == "rustc" || p_file_name == "cargo" ||
              p_file_name == "make" || p_file_name == "pacman"
            ;
            if heavy_p_running {
              want_high_cpu = true;
              break;
            }
          }
        }
      }
    }

    if want_high_cpu && !have_high_cpu {
      on_wanted_cpu_level(CPU_GOV_PERFORMANCE).await;
      have_high_cpu = true;
    }
    else if !want_high_cpu && have_high_cpu {
      on_wanted_cpu_level(CPU_GOV_ONDEMAND).await;
      have_high_cpu = false;

    }

  }
}


// anything < 4 is considered is an invalid value for PIDs,
// anything > 4 will be paused. If a request for a new PID to be
// paused comes in and all slots are full the request is ignored.
static PAUSED_PROC_PIDS: once_cell::sync::Lazy<[std::sync::atomic::AtomicI32; 16]> = once_cell::sync::Lazy::new(|| [
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

async fn unpause_pid(pid: i32) {
  // For all slots, set to 0 if == pid
  for i in 0..PAUSED_PROC_PIDS.len() {
    if PAUSED_PROC_PIDS[i].load(std::sync::atomic::Ordering::Relaxed) == pid {
      PAUSED_PROC_PIDS[i].store(0, std::sync::atomic::Ordering::Relaxed);
    }
  }
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
  let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(4));
  loop {
    interval.tick().await;

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
    tokio::time::sleep( std::time::Duration::from_millis(250) ).await;

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
  let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(6));
  
  interval.tick().await;

  let external_scratch_usb_disk = "/dev/disk/by-partuuid/e08214f5-cfc5-4252-afee-505dfcd23808";
  let internal_sd_card = "/dev/disk/by-partuuid/8f3ca68c-d031-2d41-849c-be5d9602e920";


  let swap_file_dir: std::path::PathBuf = match get_mount_pt_of(external_scratch_usb_disk).await {
    Some(mount_pt) => {
      mount_pt.join("swap-files")
    }
    None => {
      match get_mount_pt_of(internal_sd_card).await {
        Some(mount_pt) => {
          mount_pt.join("swap-files")
        }
        None => {
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
    interval.tick().await;

    // See https://docs.rs/nix/latest/nix/sys/sysinfo/struct.SysInfo.html
    //let info = nix::sys::sysinfo();
    if let Ok(info) = nix::sys::sysinfo::sysinfo() {

      let total_swap_bytes = info.swap_total();
      let free_swap_bytes = info.swap_free();
      let used_swap_bytes = total_swap_bytes - free_swap_bytes;

      let swap_fraction_used: f64 = used_swap_bytes as f64 / total_swap_bytes as f64;
      if swap_fraction_used > 0.60 && num_swap_files_added < 20 /* at 80gb of swap we have other problems */ {
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
      else if swap_fraction_used < 0.25 && num_swap_files_added > 0 {
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

      // // Also test if system is under load & if do bump CPU?
      let (one_m, five_m, fifteen_m) = info.load_average();
      if one_m > 3.25 {
        // If > 3 cores are saturated, try to go to high performance
        on_wanted_cpu_level(CPU_GOV_PERFORMANCE).await;
      }
      else if one_m < 0.60 {
        // if <1 core used go low
        on_wanted_cpu_level(CPU_GOV_POWERSAVE).await;
      }


    }

  }
}








async fn get_mount_pt_of(device_path: &str) -> Option<std::path::PathBuf> {
  if let Ok(device_path) = tokio::fs::canonicalize(device_path).await {
    if let Ok(info) = mountinfo::MountInfo::new() {
      for mount_pt in info.mounting_points {
        //println!("mount_pt={:?}", mount_pt);
        if std::path::PathBuf::from(mount_pt.what) == device_path {
          return Some(mount_pt.path);
        }
      }
    }
  }
  return None;
}

async fn is_mounted(directory_path: &str) -> bool {
  if let Ok(info) = mountinfo::MountInfo::new() {
    return info.is_mounted(directory_path);
  }
  return false;
}

async fn set_cpu(governor: &str) {
  println!("setting CPU to {}", governor);
  dump_error_and_ret!(
    tokio::process::Command::new("sudo")
      .args(&["cpupower", "frequency-set", "-g", governor])
      .status()
      .await
  );
}

// Trait lifetime gymnastics want &'static lifetimes, we'll give them &'static lifetimes!
pub static CPU_GOV_CONSERVATIVE : &'static str = "conservative";
pub static CPU_GOV_ONDEMAND     : &'static str = "ondemand";
pub static CPU_GOV_USERSPACE    : &'static str = "userspace";
pub static CPU_GOV_POWERSAVE    : &'static str = "powersave";
pub static CPU_GOV_PERFORMANCE  : &'static str = "performance";
pub static CPU_GOV_SCHEDUTIL    : &'static str = "schedutil";
pub static CPU_GOV_UNK          : &'static str = "UNK";

async fn get_cpu() -> &'static str {
  if let Ok(contents) = tokio::fs::read_to_string("/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor").await {
    let contents = contents.trim();
    if contents.contains(CPU_GOV_CONSERVATIVE) {
      return CPU_GOV_CONSERVATIVE;
    }
    else if contents.contains(CPU_GOV_ONDEMAND) {
      return CPU_GOV_ONDEMAND;
    }
    else if contents.contains(CPU_GOV_USERSPACE) {
      return CPU_GOV_USERSPACE;
    }
    else if contents.contains(CPU_GOV_POWERSAVE) {
      return CPU_GOV_POWERSAVE;
    }
    else if contents.contains(CPU_GOV_PERFORMANCE) {
      return CPU_GOV_PERFORMANCE;
    }
    else if contents.contains(CPU_GOV_SCHEDUTIL) {
      return CPU_GOV_SCHEDUTIL;
    }
  }
  return CPU_GOV_UNK;
}





