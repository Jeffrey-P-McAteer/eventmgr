use futures::prelude::*;
//use tokio::prelude::*;

type AnyError<T> = Result<T, Box<dyn std::error::Error>>;

fn main() {
  // Runtime spawns an i/o thread + others + manages task dispatch for us
  let mut rt = tokio::runtime::Runtime::new().unwrap();
  let future = eventmgr();
  rt.block_on(future);
}


async fn eventmgr() {
  println!("Beginning eventmgr event loop...");
  let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(2));
  let mut sig_usr1_stream = tokio::signal::unix::signal( tokio::signal::unix::SignalKind::user_defined1() ).expect("Could not bind to SIGUSR1");
  
  loop {
    let _ = tokio::select!{
      _ = interval.tick() => {},
      _ = sig_usr1_stream.recv() => {},
    };

    println!("Tick!");

    let (r1, r2) = tokio::join!(
      poll_downloads(),
      poll_downloads(),
    );

    print_errors(&[r1, r2]).await;

  }
}

async fn print_errors<'a, T, V: 'a>(results: T)
  where
    T: IntoIterator<Item = &'a AnyError<V> >,
{
  for result in results {
    if let Err(e) = result {
      eprintln!("ERROR> {:?}", e);
    }
  }
}

async fn poll_downloads() -> AnyError<()> {
  
  let mut entries = tokio::fs::read_dir("/j/downloads").await?;

  while let Some(entry) = entries.try_next().await? {
      println!("dl> {}", entry.file_name().to_string_lossy());
  }

  Ok(())
}



