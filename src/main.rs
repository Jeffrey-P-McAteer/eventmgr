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
  
  loop {
    interval.tick().await; // Ticks every 2 seconds, or less time if we spent time doing tasks below.

    let (r1, r2, r3) = tokio::join!(
      poll_downloads(),
      poll_downloads(),
      poll_downloads(),
    );

    print_errors(&[r1, r2, r3]).await;

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



