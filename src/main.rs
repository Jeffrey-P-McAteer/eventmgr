
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
  if args.len() > 1 {
    event_client(&args);
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

fn event_client(args: &Vec<String>) {
  if args.contains(&"install".to_string()) {
    install_self();
    return;
  }

  if run_local_event_client(args) {
    // Able to process request locally (usually key press args)
    return;
  }

  let mut msg_bytes = Vec::new();
  dump_error!(
    ciborium::ser::into_writer(&args, &mut msg_bytes)
  );

  let (client_sock, _unused_sock) = std::os::unix::net::UnixDatagram::pair().expect("Could not make socket pair!");
  dump_error!(
    client_sock.send_to(&msg_bytes, SERVER_SOCKET)
  );
}


fn run_local_event_client(args: &Vec<String>) -> bool {
  
  if args.contains(&"brightness-down".to_string()) || args.contains(&"brightness-up".to_string()) {
    let brightness_multiplier: f64;
    if args.contains(&"brightness-down".to_string()) {
      brightness_multiplier = 0.80;
    }
    else {
      brightness_multiplier = 1.25;
    }
    if let Ok(monitors) = bulbb::monitor::MonitorDevice::get_all_monitor_devices() {
      for monitor in monitors {
        let current_brightness = monitor.get_brightness();
        println!("current_brightness={}", current_brightness);
        let mut new_brightness = (current_brightness as f64 * brightness_multiplier) as u32;

        if new_brightness == current_brightness {
          if brightness_multiplier < 1.0 {
            if new_brightness > 0 {
              new_brightness -= 1;
            }
          }
          else {
            new_brightness += 1;
          }
        }
        
        if new_brightness < 1 {
          new_brightness = 1;
        }

        println!("new_brightness={}", new_brightness);
        dump_error!(
          monitor.set_brightness(new_brightness)
        );
      }
    }

    return true;
  }


  if args.contains(&"kbd-on".to_string()) || args.contains(&"kbd-off".to_string()) {
    let want_kbd_on = args.contains(&"kbd-on".to_string());
    if let Ok(led_devices) = bulbb::misc::LedDevice::get_all_led_devices() {
      for ld in led_devices {
        if let Some(bulbb::misc::LedFunction::KbdBacklight) = ld.info.function {
          
          if want_kbd_on {
            dump_error!( ld.set_brightness(1) );
          }
          else {
            dump_error!( ld.set_brightness(0) );
          }

        }
      }
    }
    return true;
  }


  return false;
}

fn install_self() {
  // Assume we are running as root + write directly to service file
  let install_service_file = "/etc/systemd/system/eventmgr.service";
  let install_service_str = format!(r#"
[Unit]
Description=Jeff's event manager
StartLimitIntervalSec=0

[Service]
Type=simple
Restart=always
RestartSec=1
User=jeffrey
ExecStart={exe}
RuntimeMaxSec=90m

[Install]
WantedBy=multi-user.target
"#, exe=dump_error_and_ret!(std::env::current_exe()).to_string_lossy() );
  println!();
  println!("Installing to {}", install_service_file);
  println!("{}", install_service_str);
  println!();
  dump_error!(
    std::fs::write(install_service_file, install_service_str.as_bytes())
  );
  dump_error_and_ret!(
    std::process::Command::new("sudo")
      .args(&["-n", "systemctl", "daemon-reload"])
      .status()
  );
  dump_error_and_ret!(
    std::process::Command::new("sudo")
      .args(&["-n", "systemctl", "stop", "eventmgr"])
      .status()
  );
  dump_error_and_ret!(
    std::process::Command::new("sudo")
      .args(&["-n", "systemctl", "enable", "--now", "eventmgr"])
      .status()
  );
  println!("Installed!");
}


async fn eventmgr() {
  notify("Beginning eventmgr event loop...").await;

  let mut tasks = vec![
    PersistentAsyncTask::new("handle_exit_signals",            ||{ tokio::task::spawn(handle_exit_signals()) }),
    PersistentAsyncTask::new("handle_sway_msgs",               ||{ tokio::task::spawn(handle_sway_msgs()) }),
    PersistentAsyncTask::new("handle_socket_msgs",             ||{ tokio::task::spawn(handle_socket_msgs()) }),
    PersistentAsyncTask::new("poll_downloads",                 ||{ tokio::task::spawn(poll_downloads()) }),
    PersistentAsyncTask::new("poll_ff_bookmarks",              ||{ tokio::task::spawn(poll_ff_bookmarks()) }),
    PersistentAsyncTask::new("poll_check_dexcom",              ||{ tokio::task::spawn(poll_check_dexcom()) }),
    PersistentAsyncTask::new("mount_disks",                    ||{ tokio::task::spawn(mount_disks()) }),
    PersistentAsyncTask::new("bump_cpu_for_performance_procs", ||{ tokio::task::spawn(bump_cpu_for_performance_procs()) }),

  ];

  // We check for failed tasks and re-start them every 6 seconds
  let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(6));
  
  loop {
    interval.tick().await;

    for i in 0..tasks.len() {
      tasks[i].ensure_running();
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
      .timeout(notify_rust::Timeout::Milliseconds(6000)) //milliseconds
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
    tokio::select!{
      _sig_int = int_stream.recv() => {
        println!("Got SIGINT, shutting down!");

        if let Ok(mut last_focus_task) = WINDOW_FOCUS_CPU_TASK.lock() {
          let taken_task = last_focus_task.take();
          std::mem::drop(taken_task);
        }

        // Allow spawned futures to complete...
        tokio::time::sleep( tokio::time::Duration::from_millis(750) ).await;

        println!("Goodbye!");

        std::process::exit(0);
      }
      _sig_term = term_stream.recv() => {
        println!("Got SIGTERM, shutting down!");

        if let Ok(mut last_focus_task) = WINDOW_FOCUS_CPU_TASK.lock() {
          let taken_task = last_focus_task.take();
          std::mem::drop(taken_task);
        }

        // Allow spawned futures to complete...
        tokio::time::sleep( tokio::time::Duration::from_millis(750) ).await;

        println!("Goodbye!");

        std::process::exit(0);
      }
    };
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



static WINDOW_FOCUS_CPU_TASK: once_cell::sync::Lazy<std::sync::Mutex< Option<UndoableTask> >> = once_cell::sync::Lazy::new(||
  std::sync::Mutex::new( None )
);

async fn on_window_focus(window_name: &str) {
  println!("Window focused = {:?}", window_name );
  
  // Always undo last task on focus change
  if let Ok(mut last_focus_task) = WINDOW_FOCUS_CPU_TASK.lock() {
    /*let taken_task = last_focus_task.take(); // Puts None in there
    if let Some(mut task) = taken_task {
      task.undo_it();
    }*/
    // .undo_it() called in Drop, so guaranteed to run! \o/
    let _taken_task = last_focus_task.take();
  }

  let lower_window = window_name.to_lowercase();
  if lower_window.contains("team fortress") && lower_window.contains("opengl") {
    // Highest performance!
    let current_cpu = get_cpu();
    if let Ok(mut last_focus_task) = WINDOW_FOCUS_CPU_TASK.lock() {
      let mut task = UndoableTask::create(
             || { tokio::task::spawn(set_cpu(CPU_GOV_PERFORMANCE)); },
        move || { tokio::task::spawn(set_cpu(current_cpu)); },
      );
      task.do_it();
      *last_focus_task = Some(task);
    }
  }
  else if lower_window.contains("mozilla firefox") {
    // Go to ondemand
    let current_cpu = get_cpu();
    if let Ok(mut last_focus_task) = WINDOW_FOCUS_CPU_TASK.lock() {
      let mut task = UndoableTask::create(
             || { tokio::task::spawn(set_cpu(CPU_GOV_ONDEMAND)); },
        move || { tokio::task::spawn(set_cpu(current_cpu)); },
      );
      task.do_it();
      *last_focus_task = Some(task);
    }
  }


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


async fn poll_ff_bookmarks() {
  let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(60));
  loop {
    interval.tick().await;

    // See https://crates.io/crates/marionette

    
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





static MOUNT_DISKS: phf::Map<&'static str, (&'static str, &'static str) > = phf::phf_map! {
  "/dev/disk/by-partuuid/53da446a-2409-ca42-8337-12389dc70563" => 
    ("/mnt/scratch", "auto,rw,noatime,data=writeback,barrier=0,nobh,errors=remount-ro"),

  "/dev/disk/by-partuuid/435cfadf-6a6e-4acf-a784-ab3f792ee8c6" => 
    ("/mnt/wda", "auto,rw"),

  "/dev/disk/by-partuuid/ee209a96-9170-534a-9ba2-ea0a34ac156e" => 
    ("/mnt/wdb", "auto,rw"),

};

async fn mount_disks() {
  let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(4));

  loop {
    interval.tick().await;

    for (disk_block_device, (disk_mount_path, disk_mount_opts)) in MOUNT_DISKS.entries() {
      if std::path::Path::new(disk_block_device).exists() {
        if ! is_mounted(disk_mount_path).await {
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



async fn bump_cpu_for_performance_procs() {
  let mut interval = tokio::time::interval(tokio::time::Duration::from_millis(1200));

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
      notify("Want high CPU!").await;
      have_high_cpu = true; // pretend we changed something
    }
    else if !want_high_cpu && have_high_cpu {
      notify("Set low/notmal CPU!").await;
      have_high_cpu = false;

    }

  }
}

static PAUSED_PROC_PIDS: once_cell::sync::Lazy<std::sync::Mutex< Vec<usize> >> = once_cell::sync::Lazy::new(||
  std::sync::Mutex::new( vec![] )
);

async fn pause_pid(pid: usize) {
  if let Ok(mut proc_pids) = PAUSED_PROC_PIDS.lock() {
    dump_error_and_ret!(
      nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(pid as i32), nix::sys::signal::Signal::SIGSTOP
      )
    );
    proc_pids.push(pid);
  }
}

async fn partial_resume_paused_procs() {
  let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(2));
  loop {
    interval.tick().await;

    if let Ok(mut proc_pids) = PAUSED_PROC_PIDS.lock() {
      let mut pids_to_remove_from_list: Vec<usize> = vec![];
      for proc_pid in proc_pids.iter() {
        if let Err(_e) = procfs::process::Process::new(*proc_pid as i32) {
          pids_to_remove_from_list.push( *proc_pid );
          continue;
        }
        dump_error!(
          nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(*proc_pid as i32), nix::sys::signal::Signal::SIGSTOP
          )
        );
      }
      // Finally drain vec of values in proc_pids
      for pid_to_remove in pids_to_remove_from_list {
        if let Some(pos) = proc_pids.iter().position(|x| *x == pid_to_remove) {
          proc_pids.remove(pos);
        }
      }
    }

  }
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
static CPU_GOV_CONSERVATIVE : &'static str = "conservative";
static CPU_GOV_ONDEMAND     : &'static str = "ondemand";
static CPU_GOV_USERSPACE    : &'static str = "userspace";
static CPU_GOV_POWERSAVE    : &'static str = "powersave";
static CPU_GOV_PERFORMANCE  : &'static str = "performance";
static CPU_GOV_SCHEDUTIL    : &'static str = "schedutil";
static CPU_GOV_UNK          : &'static str = "UNK";

fn get_cpu() -> &'static str {
  if let Ok(contents) = std::fs::read_to_string("/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor") {
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

pub struct UndoableTask {
  pub do_task: Box<dyn FnMut() -> () + Send>,
  pub undo_task: Box<dyn FnMut() -> () + Send>,
}

impl UndoableTask {
  pub fn create<F1, F2>(do_task: F1, undo_task: F2 ) -> UndoableTask
    where 
      F1: FnMut() -> () + Send + 'static,
      F2: FnMut() -> () + Send + 'static
  {
    UndoableTask {
      do_task: Box::new(do_task),
      undo_task: Box::new(undo_task),
    }
  }
  pub fn do_it(&mut self) {
    (self.do_task)();
  }
  pub fn undo_it(&mut self) {
    (self.undo_task)();
  }
}

impl Drop for UndoableTask {
  fn drop(&mut self) {
    self.undo_it(); // If process crashes, this ought to run
  }
}

pub struct PersistentAsyncTask {
  pub name: String,
  pub spawn_fn: Box<dyn FnMut() -> tokio::task::JoinHandle<()> + Send>,
  pub running_join_handle: Option< tokio::task::JoinHandle<()> >,
}

impl PersistentAsyncTask {
  pub fn new<F>(name: &str, spawn_fn: F) -> PersistentAsyncTask
    where 
        F: FnMut() -> tokio::task::JoinHandle<()> + Send + 'static
  {
    PersistentAsyncTask {
      name: name.to_string(),
      spawn_fn: Box::new(spawn_fn),
      running_join_handle: None
    }
  }

  pub async fn ensure_running(&mut self) {
    let need_spawn = if let Some(ref handle) = &self.running_join_handle { handle.is_finished() } else { true };
    if need_spawn {
      notify(format!("re-starting {}", self.name).as_str()).await;
      self.running_join_handle = Some( (self.spawn_fn)() );
    }
  }

}





