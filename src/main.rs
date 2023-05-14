
use futures::prelude::*;

type AnyError<T> = Result<T, Box<dyn std::error::Error>>;

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
  let mut rt = tokio::runtime::Runtime::new().unwrap();
  if args.len() > 1 {
    rt.block_on(event_client(&args));
  }
  else {
    rt.block_on(eventmgr());
  }
}

async fn event_client(args: &Vec<String>) {
  let mut msg_bytes = Vec::new();
  dump_error!(
    ciborium::ser::into_writer(&args, &mut msg_bytes)
  );

  let (mut client_sock, _unused_sock) = tokio::net::UnixDatagram::pair().expect("Could not make socket pair!");
  dump_error_async!(
    client_sock.send_to(&msg_bytes, SERVER_SOCKET)
  ).await;
}


async fn eventmgr() {
  println!("Beginning eventmgr event loop...");

  let mut handle_socket_msgs_fut = tokio::task::spawn(handle_socket_msgs());
  let mut poll_downloads_fut = tokio::task::spawn(poll_downloads());
  let mut mount_disks_fut = tokio::task::spawn(mount_disks());

  // We check for failed tasks and re-start them every 4 seconds
  let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(6));
  
  loop {
    interval.tick().await;
    
    // Re-start any jobs that failed (future returned error or something)
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

async fn handle_socket_msgs() {
  if std::path::Path::new(SERVER_SOCKET).exists() {
    dump_error_and_ret!(
      std::fs::remove_file(SERVER_SOCKET)
    );
  }

  let mut server = dump_error_and_ret!( tokio::net::UnixDatagram::bind(SERVER_SOCKET) );
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


pub const DOWNLOAD_QUARANTINE_SECS: u64 = 36 * 60 * 60;

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
              println!("{} needs to be quarantined!", entry.file_name().to_string_lossy());
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



