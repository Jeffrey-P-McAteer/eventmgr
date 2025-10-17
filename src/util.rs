
use crate::macros::*;

pub async fn get_mount_pt_of(info: &mountinfo::MountInfo, device_path: &str) -> Option<std::path::PathBuf> {
  if let Ok(device_path) = tokio::fs::canonicalize(device_path).await {
    for mount_pt in &info.mounting_points {
      //println!("mount_pt={:?}", mount_pt);
      if std::path::PathBuf::from(mount_pt.what.clone()) == device_path {
        return Some( mount_pt.path.clone() );
      }
    }
  }
  return None;
}

pub async fn is_mounted(info: &mountinfo::MountInfo, directory_path: &str) -> bool {
  return info.is_mounted(directory_path);
}

pub async fn set_cpu(governor: &str) {
  println!("setting CPU to {}", governor);
  dump_error_and_ret!(
    tokio::process::Command::new("sudo")
      .args(&["-n", "cpupower", "frequency-set", "-g", governor])
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

pub async fn get_cpu() -> &'static str {
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

pub async fn is_lid_closed(acpi_path: &str) -> bool {

  match tokio::fs::read_to_string(acpi_path).await {
    Ok(contents) => {
      let lower_contents = contents.to_lowercase();
      // Typical content: "state:      open" or "state:      closed"
      if lower_contents.contains("closed") {
          return true;
      }
      else if lower_contents.contains("open") {
          return false;
      }
      else {
        dump_any!(format!("Un-handled file contents: {} were '{}'", acpi_path, contents));
      }
    }
    Err(e) => {
      dump_any!(e);
    }
  }
  return false;
}

pub async fn blink_lid_thinkpad_led(pattern: &[bool]) {
  for go_on in pattern {
    if *go_on {
      set_lid_thinkpad_led("1\n").await;
    }
    else {
      set_lid_thinkpad_led("0\n").await;
    }
    tokio::time::sleep( tokio::time::Duration::from_millis(100) ).await;
  }
}

pub async fn set_lid_thinkpad_led(content: &str) {
  const CONTROL_FILE: &'static str = "/sys/devices/platform/thinkpad_acpi/leds/tpacpi::lid_logo_dot/brightness";
  write_to_sysfs_file(CONTROL_FILE, content).await;
  // dump_error_and_ret!(
  //   tokio::process::Command::new("sudo")
  //     .args(&["-n", "sh", "-c", format!("echo '{}' > {}", content, CONTROL_FILE).as_str(), ])
  //     .status()
  //     .await
  // );
}


pub async fn blink_power_thinkpad_led(pattern: &[bool]) {
  for go_on in pattern {
    if *go_on {
      set_power_thinkpad_led("1\n").await;
    }
    else {
      set_power_thinkpad_led("0\n").await;
    }
    tokio::time::sleep( tokio::time::Duration::from_millis(100) ).await;
  }
}

pub async fn set_power_thinkpad_led(content: &str) {
  const CONTROL_FILE: &'static str = "/sys/devices/platform/thinkpad_acpi/leds/tpacpi::power/brightness";
  write_to_sysfs_file(CONTROL_FILE, content).await;
  // dump_error_and_ret!(
  //   tokio::process::Command::new("sudo")
  //     .args(&["-n", "sh", "-c", format!("echo '{}' > {}", content, CONTROL_FILE).as_str(), ])
  //     .status()
  //     .await
  // );
}

// Efficiency hack; we write to the file directly, and if we fail we sudo chmod a+rw it and re-try.
pub async fn write_to_sysfs_file(path: &'static str, content: &str) {
  match tokio::fs::write(path, content.as_bytes()).await {
    Ok(_) => { },
    Err(e) => {
      dump_error_and_ret!(
        tokio::process::Command::new("sudo")
          .args(&["-n", "chmod", "a+rw", path, ])
          .status()
          .await
      );
      match tokio::fs::write(path, content.as_bytes()).await {
        Ok(_) => { },
        Err(e) => {
          dump_any!(e);
          dump_error_and_ret!(
            tokio::process::Command::new("sudo")
              .args(&["-n", "sh", "-c", format!("echo '{}' > {}", content, path).as_str(), ])
              .status()
              .await
          );
        }
      }
    }
  }
}
