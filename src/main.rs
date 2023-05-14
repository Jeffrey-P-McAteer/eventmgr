use futures::prelude::*;
//use tokio::prelude::*;

type AnyError<T> = Result<T, Box<dyn std::error::Error>>;

pub const server_socket: &'static str = "/tmp/eventmgr.sock";

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
  println!("TODO process args={:?}", args);
  
  let mut msg_bytes = Vec::new();
  print_errors(&[
    ciborium::ser::into_writer(&args, &mut msg_bytes)
  ]).await;

  let (mut client_sock, _unused_sock) = tokio::net::UnixDatagram::pair().expect("Could not make socket pair!");
  print_errors(&[
    client_sock.send_to(&msg_bytes, server_socket).await
  ]).await;


}


async fn eventmgr() {
  println!("Beginning eventmgr event loop...");
  let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(4));
  if std::path::Path::new(server_socket).exists() {
    print_errors(&[
      std::fs::remove_file(server_socket)
    ]).await;
  }
  let mut server = tokio::net::UnixDatagram::bind(server_socket).expect("Could not bind to server socket!");
  let mut msg_buf = vec![0u8; 4096];
  let mut msg_size: usize = 0;

  loop {
    let _ = tokio::select!{
      _ = interval.tick() => {},
      res = server.recv_from(&mut msg_buf) => {
        if let Ok((size, addr)) = res {
          msg_size = size;
        }
      },
    };

    println!("Tick!");

    if msg_size > 0 {
      // Handle message!
      let msg_bytes = &msg_buf[0..msg_size];
      match ciborium::de::from_reader::<ciborium::Value, &[u8]>(msg_bytes) {
        Ok(msg_val) => {
          println!("Got message: {:?}", &msg_val );
        }
        Err(e) => {
          eprintln!("CBOR decoding error: {:?}", e);
        }
      }
      // clear message buffer
      msg_size = 0;
    }

    let (r1, r2) = tokio::join!(
      poll_downloads(),
      poll_downloads(),
    );

    print_errors(&[r1, r2]).await;

  }
}


async fn print_errors<'a, T, V: 'a, E: 'a + std::fmt::Debug>(results: T)
  where
    T: IntoIterator<Item = &'a Result<V, E> >,
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



