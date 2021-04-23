use std::{
    future::Future,
    pin::Pin,
    sync::{
        atomic::{AtomicU8, Ordering},
        Arc,
    },
};

static COUNT: u8 = 8;

async fn feeder(sender: async_channel::Sender<Option<u8>>) {
    let done = Arc::new(AtomicU8::new(0));
    let mut tasks = Vec::new();
    for i in 1..=COUNT {
        let sender = sender.clone();
        let done = done.clone();
        tasks.push(uexec::spawn(async move {
            println!("Send {}", i);
            drop(sender.send(Some(i)).await);
            if done.fetch_add(1, Ordering::SeqCst) == COUNT {
                // We're the last one, close the stream
                drop(sender.send(None).await);
            }
        }));
    }
    for task in tasks {
        task.await;
    }
}

async fn eater(receiver: async_channel::Receiver<Option<u8>>) -> u8 {
    let mut r = 0u8;
    while let Ok(Some(data)) = receiver.recv().await {
        println!("Received {}", data);
        r += data;
    }
    r
}

fn main() {
    uexec::block_on(async {
        let mut tasks: Vec<Pin<Box<dyn Future<Output = ()> + Send>>> = Vec::new();
        let (sender, receiver) = async_channel::unbounded();
        tasks.push(Box::pin(uexec::spawn(feeder(sender))));
        tasks.push(Box::pin(uexec::spawn(async move {
            let res = eater(receiver).await;
            println!("Finished with res {}", res);
            assert_eq!(res, (1..=COUNT).sum());
        })));
        for task in tasks {
            task.await;
        }
    });
}
