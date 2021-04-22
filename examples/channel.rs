use std::sync::{Arc, atomic::{AtomicU8, Ordering}};

static COUNT: u8 = 8;

async fn feeder(sender: async_channel::Sender<Option<u8>>) {
    let done = Arc::new(AtomicU8::new(0));
    for i in 1..=COUNT {
        let sender = sender.clone();
        let done = done.clone();
        uexec::spawn(async move {
            println!("Send {}", i);
            drop(sender.send(Some(i)).await);
            if done.fetch_add(1, Ordering::SeqCst) == COUNT {
                // We're the last one, close the stream
                drop(sender.send(None).await);
            }
        });
    }
}

fn main() {
    let res = uexec::block_on(async {
        let mut r = 0u8;
        let (sender, receiver) = async_channel::unbounded();
        uexec::spawn(feeder(sender));
        while let Ok(Some(data)) = receiver.recv().await {
            println!("Received {}", data);
            r += data;
        }
        r
    });
    println!("Finished with res {}", res);
    assert_eq!(res, (1..=COUNT).sum());
}
