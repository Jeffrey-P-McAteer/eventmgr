
use crate::*;

pub fn event_client(args: &Vec<String>) {
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


pub fn run_local_event_client(args: &Vec<String>) -> bool {

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


  if args.contains(&"volume-up".to_string()) || args.contains(&"volume-down".to_string()) || args.contains(&"volume-mute-toggle".to_string()) {
    let want_mute_toggle = args.contains(&"volume-mute-toggle".to_string());
    let want_vol_up = args.contains(&"volume-up".to_string());

    // We assume wireplumber is installed
    if want_mute_toggle {
      dump_error!(
        std::process::Command::new("wpctl")
          .args(&["set-mute", "@DEFAULT_AUDIO_SINK@", "toggle"])
          .status()
      );
    }
    else {

      let mut curr_volume: f64 = -1.0;

      if let Ok(curr_vol_cmd_o) = std::process::Command::new("wpctl")
            .args(&["get-volume", "@DEFAULT_AUDIO_SINK@"])
            .output()
      {
        let vol_str = String::from_utf8_lossy(&curr_vol_cmd_o.stdout);
        let words: Vec<&str>= vol_str.split(' ').collect();
        println!("words = {:?}", words);
        if words.len() > 1 {
          let number_word = words[1].trim();
          if let Ok(vol_num) = number_word.parse::<f64>() {
            curr_volume = vol_num;
          }
        }
      }

      if want_vol_up {
        let _ = notify_sync(format!("Volume {}%", (curr_volume * 105.0).round() ).as_str());
        dump_error!(
          std::process::Command::new("wpctl")
            .args(&["set-volume", "-l", "1.5", "@DEFAULT_AUDIO_SINK@", "5%+"])
            .status()
        );
      }
      else {
        let _ = notify_sync(format!("Volume {}%", (curr_volume * 95.0).round() ).as_str());
        dump_error!(
          std::process::Command::new("wpctl")
            .args(&["set-volume", "@DEFAULT_AUDIO_SINK@", "5%-"])
            .status()
        );
      }
    }

    return true;
  }


  if args.contains(&"do-lock".to_string()) {
    // If we aren't supposed to lock, don't.
    if std::path::Path::new("/tmp/no-lock").exists() {
      std::thread::sleep(std::time::Duration::from_millis(30 * 1000));
      return true;
    }
    // If anything is playing audio, assume a video or a game is being played and refuse to lock
    if std::path::Path::new("/tmp/eventmgr-audio-is-playing").exists() {
      std::thread::sleep(std::time::Duration::from_millis(30 * 1000));
      return true;
    }


    // Take screenshot + blur it
    dump_error!(
      std::process::Command::new("grim")
        .args(&["-g", "0,0 1920x1080", "-l", "1", "/tmp/lock-screen.png"])
        .status()
    );
    // See https://stackoverflow.com/questions/35649413/imagemagick-looking-for-a-fast-way-to-blur-an-image
    dump_error!(
      std::process::Command::new("convert")
        .args(&["-scale", "10%", "-blur", "0x1.1", "-resize", "1000%", "/tmp/lock-screen.png", "/tmp/lock-screen-blurred.png"])
        .status()
    );
    // Set CPU low
    dump_error!(
      std::process::Command::new("sudo")
        .args(&["cpupower", "frequency-set", "-g", crate::CPU_GOV_POWERSAVE ])
        .status()
    );
    // Remove any high-cpu file if exists
    dump_error!(
      std::fs::remove_file("/tmp/force-cpu-performance")
    );
    dump_error!(
      std::fs::write("/tmp/force-cpu-powersave", "-")
    );
    // Lock screen
    dump_error!(
      std::process::Command::new("swaylock")
        .args(&[
          "-i", "/tmp/lock-screen-blurred.png",
          "--indicator-radius", "120",

        ])
        .status()
    );
    // Ensure screen is turned back on if idle for a long time
    dump_error!(
      std::process::Command::new("swaymsg")
        .args(&["output * dpms on"])
        .status()
    );
    // Remove low-cpu file
    dump_error!(
      std::fs::remove_file("/tmp/force-cpu-powersave")
    );
    return true;
  }


  return false;
}

pub fn install_self() {
  // Assume we are running as root + write directly to service file
  let install_service_file = "/etc/systemd/system/eventmgr.service";
  let install_service_str = format!(r#"[Unit]
Description=Jeff's event manager
StartLimitIntervalSec=0

[Service]
Type=simple
Restart=always
RestartSec=1
User=jeffrey
ExecStart={exe}
RuntimeMaxSec=300m
StandardError=journal
StandardOutput=journal
StandardInput=null
TimeoutStopSec=4

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




