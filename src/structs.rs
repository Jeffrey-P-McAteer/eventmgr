

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
      crate::notify(format!("re-starting {}", self.name).as_str()).await;
      self.running_join_handle = Some( (self.spawn_fn)() );
    }
  }

}
