
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


// Maps intel / sysfs brightness ranges to a list of acceptable
// ddcutil brightness ranges.
pub static BRIGHTNESS_DDCUTIL_MAP: &[((u32, u32), (u32, u32))] = &[
  ((0, 450),     (1, 1)),
  ((450, 2000),  (1, 6)),
  ((2000, 5000), (6, 26)),
  ((5000, 25000), (26, 100)),
];



pub fn run_local_event_client(args: &Vec<String>) -> bool {

  if args.contains(&"brightness-down".to_string()) || args.contains(&"brightness-up".to_string()) {

    let mut wanted_ddcutil_brightness_val: Option<u32> = None;

    let brightness_multiplier: f64;
    if args.contains(&"brightness-down".to_string()) {
      brightness_multiplier = 0.75;
    }
    else {
      brightness_multiplier = 1.30;
    }

    let mut intel_set_brightness = 999;

    // Adjust all devices which present under /sys
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
        if new_brightness > 24242 {
          new_brightness = 24242; // For intel monitor
        }

        println!("new_brightness={}", new_brightness);

        intel_set_brightness = new_brightness;

        if let Err(e) = monitor.set_brightness(new_brightness) {
          println!("Error setting brightness: {:?}", e);
          dump_error!( // Give everyone write access
            std::process::Command::new("sudo")
              .args(&["sh", "-c", "chmod a+rw /sys/class/backlight/*/*"])
              .status()
          );
        }

        for ((begin_b, end_b), (ddc_begin_b, ddc_end_b)) in BRIGHTNESS_DDCUTIL_MAP {
          if new_brightness >= *begin_b && new_brightness <= *end_b {
            let range = end_b - begin_b;
            let fraction_of_range = (new_brightness - begin_b) as f32 / range as f32;
            let ddc_range = ddc_end_b - ddc_begin_b;
            let ddc_used_range = (ddc_range as f32 * fraction_of_range) as u32;

            wanted_ddcutil_brightness_val = Some(ddc_begin_b + ddc_used_range);

            break;
          }
        }

      }
    }


    // Also adjust ddcutil devices
    let ddcutil_serials = [
      "PTBLAJA000229",
    ];
    for ddcutil_serial in ddcutil_serials.iter() {
      let exists_flag_file = format!("/tmp/.ddcutil_notpresent_{}", ddcutil_serial);

      if std::path::Path::new(&exists_flag_file).exists() {
        // 9 out of 10 times, exit!
        let rand_num = fastrand::usize(0..100);
        if rand_num < 95 {
          continue;
        }
      }

      println!("wanted_ddcutil_brightness_val = {:?}", wanted_ddcutil_brightness_val);
      if let Some(wanted_ddcutil_brightness_val) = wanted_ddcutil_brightness_val {
        let res = std::process::Command::new("ddcutil")
            .args(&["setvcp", "0x10", format!("{}", wanted_ddcutil_brightness_val).as_str(), "--sn", ddcutil_serial, "--sleep-multiplier", "0.1", "--noverify"])
            .status();
        if let Ok(exit_status) = res {
          if !exit_status.success() {
            // This indicates ddcutil_serial is not connected; log to /tmp/ as such!
            dump_error!( std::fs::write(exists_flag_file, "!") );
          }
          else if std::path::Path::new(&exists_flag_file).exists() {
            // Delete it!
            dump_error!( std::fs::remove_file(&exists_flag_file) );
          }
        }
      }

    }

    if !std::path::Path::new("/sys/kernel/btf/i2c_dev").exists() {
      dump_error!(
        std::process::Command::new("sudo")
          .args(&["-n", "modprobe", "i2c-dev"])
          .status()
      );
    }

    // if let Some(wanted_ddcutil_brightness_val) = wanted_ddcutil_brightness_val {
    //   clear_notifications_sync();
    //   // show first existing file from wanted_ddcutil_brightness_val -> 100
    //   for icon_brightness_v in (wanted_ddcutil_brightness_val-1)..120 {
    //     let icon_file = format!("/j/bins/brightness-icons/levels/{}.png", icon_brightness_v);
    //     if std::path::Path::new(&icon_file).exists() {
    //       notify_icon_only_sync(&icon_file);
    //       break;
    //     }
    //   }
    // }

    notify_sync(format!("Brightness: {:.2}", ((intel_set_brightness as f32 / 24242.0) * 100.0 ) ).as_str());

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
        notify_sync(format!("Volume {}%", (curr_volume * 105.0).round() ).as_str());
        dump_error!(
          std::process::Command::new("wpctl")
            .args(&["set-volume", "-l", "1.5", "@DEFAULT_AUDIO_SINK@", "5%+"])
            .status()
        );
      }
      else {
        notify_sync(format!("Volume {}%", (curr_volume * 95.0).round() ).as_str());
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
      std::thread::sleep(std::time::Duration::from_millis(60 * 1000));
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




