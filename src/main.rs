
use futures::prelude::*;

pub const SERVER_SOCKET: &'static str = "/tmp/eventmgr.sock";

macro_rules! dump_error {
  ($e:expr) => {
    if let Err(err) = $e {
      eprintln!("ERROR> {:?}", err);
    }
  }
}

macro_rules! dump_error_async {
  ($e:expr) => {
    async {
      if let Err(err) = $e.await {
        eprintln!("ERROR> {:?}", err);
      }
    }
  }
}

macro_rules! dump_error_and_ret {
  ($e:expr) => {
    match $e {
      Err(err) => {
        eprintln!("ERROR> {:?}", err);
        return;
      }
      Ok(val) => val
    }
  }
}

fn main() {
  // Runtime spawns an i/o thread + others + manages task dispatch for us
  let args: Vec<String> = std::env::args().collect();
  //let mut rt = tokio::runtime::Runtime::new().unwrap();
  if args.len() > 1 {
    let rt = tokio::runtime::Builder::new_current_thread()
      .enable_time()
      .build()
      .expect("Could not build tokio runtime!");

    rt.block_on(event_client(&args));
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

async fn event_client(args: &Vec<String>) {
  let mut msg_bytes = Vec::new();
  dump_error!(
    ciborium::ser::into_writer(&args, &mut msg_bytes)
  );

  let (client_sock, _unused_sock) = tokio::net::UnixDatagram::pair().expect("Could not make socket pair!");
  dump_error_async!(
    client_sock.send_to(&msg_bytes, SERVER_SOCKET)
  ).await;
}


async fn eventmgr() {
  println!("Beginning eventmgr event loop...");

  let mut handle_sway_msgs_fut = tokio::task::spawn(handle_sway_msgs());
  let mut handle_socket_msgs_fut = tokio::task::spawn(handle_socket_msgs());
  let mut poll_downloads_fut = tokio::task::spawn(poll_downloads());
  let mut mount_disks_fut = tokio::task::spawn(mount_disks());

  // We check for failed tasks and re-start them every 6 seconds
  let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(6));
  
  loop {
    interval.tick().await;
    
    if handle_sway_msgs_fut.is_finished() {
      println!("Re-starting handle_sway_msgs");
      handle_sway_msgs_fut = tokio::task::spawn(handle_sway_msgs());
    }

    if handle_socket_msgs_fut.is_finished() {
      println!("Re-starting handle_socket_msgs");
      handle_socket_msgs_fut = tokio::task::spawn(handle_socket_msgs());
    }

    if poll_downloads_fut.is_finished() {
      println!("Re-starting poll_downloads");
      poll_downloads_fut = tokio::task::spawn(poll_downloads());
    }

    if mount_disks_fut.is_finished() {
      println!("Re-starting mount_disks");
      mount_disks_fut = tokio::task::spawn(mount_disks());
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
      //println!("Sway event = {:?}", event);
      match event {
        swayipc_async::Event::Window(window_evt) => {
          if window_evt.change == swayipc_async::WindowChange::Focus {
            let name = window_evt.container.name.unwrap_or("".to_string());
            on_window_focus(&name).await;
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

async fn on_window_focus(window_name: &str) {
  println!("Window focused = {:?}", window_name );
}

async fn on_workspace_focus(workspace_name: &str) {
  println!("Workspace focused = {:?}", workspace_name );
}

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


pub const DOWNLOAD_QUARANTINE_SECS: u64 = (24 + 12) * 60 * 60;
pub const QUARANTINE_DELETE_SECS: u64 = 3 * 24 * 60 * 60;

async fn poll_downloads() {
  let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(60));
  loop {
    interval.tick().await;

    if !std::path::Path::new("/j/downloads/q").exists() {
      dump_error_and_ret!( tokio::fs::create_dir_all("/j/downloads/q").await );
    }

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

  }
}





static MOUNT_DISKS: phf::Map<&'static str, (&'static str, &'static str) > = phf::phf_map! {
  "/dev/disk/by-partuuid/53da446a-2409-ca42-8337-12389dc70563" => 
    ("/mnt/scratch", "auto,rw,noatime,data=writeback,barrier=0,nobh,errors=remount-ro"),

  "/dev/disk/by-partuuid/435cfadf-6a6e-4acf-a784-ab3f792ee8c6" => 
    ("/mnt/wda", "auto,rw"),

  "/dev/disk/by-partuuid/ee209a96-9170-534a-9ba2-ea0a34ac156e" => 
    ("/mnt/wdb", "auto,rw"),

};

async fn mount_disks() {
  let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(2));

  loop {
    interval.tick().await;

    for (disk_block_device, (disk_mount_path, disk_mount_opts)) in MOUNT_DISKS.entries() {
      if std::path::Path::new(disk_block_device).exists() {
        if ! is_mounted(disk_mount_path) {
          if ! std::path::Path::new(disk_mount_path).exists() {
            // Sudo create it, set ownership to jeffrey
            dump_error_and_ret!(
              tokio::process::Command::new("sudo")
                .args(&["-n", "mkdir", "-p", disk_mount_path])
                .status()
                .await
            );
            dump_error_and_ret!(
              tokio::process::Command::new("sudo")
                .args(&["-n", "chown", "jeffrey", disk_mount_path])
                .status()
                .await
            );
          }

          dump_error_and_ret!(
            tokio::process::Command::new("sudo")
              .args(&["-n", "mount", "-o", disk_mount_opts, disk_block_device, disk_mount_path])
              .status()
              .await
          );

        }
      }
      else {
        // Block device does NOT exist, remove mountpoint if it exists!
        if std::path::Path::new(disk_mount_path).exists() {
          dump_error_and_ret!(
            tokio::process::Command::new("sudo")
              .args(&["-n", "umount", disk_mount_path])
              .status()
              .await
          );
          dump_error_and_ret!(
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

fn is_mounted(directory_path: &str) -> bool {
  if let Ok(info) = mountinfo::MountInfo::new() {
    return info.is_mounted(directory_path);
  }
  return false;
}



